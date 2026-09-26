// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Ownable2Step} from "@openzeppelin/contracts/access/Ownable2Step.sol";
import {MerkleProof} from "@openzeppelin/contracts/utils/cryptography/MerkleProof.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";

/// @title Dedan Settlement: balances, daily cumulative roots, claims.
/// One countersigned transaction per day publishes a cumulative payout root and a cumulative bill root.
/// Payees claim with one proof however many days passed; customers withdraw after their bill is applied.
/// Roots are provisional for DISPUTE_WINDOW and can be voided by any signer or governance in that time.
/// Leaves are OpenZeppelin StandardMerkleTree leaves: keccak256(bytes.concat(keccak256(abi.encode(...)))).
contract Settlement is Ownable2Step {
    using SafeERC20 for IERC20;

    struct Account {
        address key;
        uint256 balance;
        uint256 appliedBill; // cumulative bill already deducted
        uint256 pendingWithdrawal;
        uint64 withdrawableAt;
    }

    struct Root {
        bytes32 inputsHash;
        bytes32 payoutRoot;
        bytes32 billRoot;
        uint256 totalCumPayout;
        uint256 totalCumBill;
        uint64 registryBlock;
        uint32 priceTableVersion;
        uint64 publishedAt;
        bool voided;
    }

    struct RootInput {
        uint64 period;
        bytes32 inputsHash;
        uint64 registryBlock;
        uint32 priceTableVersion;
        bytes32 payoutRoot;
        bytes32 billRoot;
        uint256 totalCumPayout;
        uint256 totalCumBill;
    }

    uint64 public constant DISPUTE_WINDOW = 24 hours;
    /// Longer than one day of service plus publication grace plus the dispute window, so the bill for
    /// every day served before the request is final by the time the withdrawal executes.
    uint64 public constant WITHDRAWAL_DELAY = 72 hours;

    IERC20 public immutable usdc;
    mapping(bytes32 id => Account) public accounts;
    mapping(address payee => uint256) public claimed;
    mapping(uint64 period => Root) public roots;
    uint64 public lastPeriod;
    uint256 public totalDeposited;
    /// @notice What customers took back; bills are bounded by `totalDeposited - totalWithdrawn`.
    uint256 public totalWithdrawn;
    address[] public signers;
    mapping(address => bool) public isSigner;
    uint8 public threshold;
    /// @notice List prices in USDC (6 decimals) per day, by price table version and tier (`tierKey`).
    mapping(uint32 version => mapping(bytes32 tier => uint256 usdcPerDay)) public prices;
    mapping(uint32 version => bool) public priceVersionSet;
    mapping(address => uint256) public bonds;
    /// @notice A signer's signature counts toward the threshold only while its bond is at least this.
    uint256 public minSignerBond;
    /// @notice Signs new account ids (the control plane), so an id is reserved to the key it names.
    address public registrar;

    event AccountCreated(bytes32 indexed id, address key);
    event Deposited(bytes32 indexed id, address from, uint256 amount);
    event WithdrawRequested(bytes32 indexed id, uint256 amount, uint64 withdrawableAt);
    event WithdrawCancelled(bytes32 indexed id);
    event Withdrawn(bytes32 indexed id, uint256 amount, uint256 billApplied);
    event Claimed(address indexed payee, uint256 amount, uint256 cumPayout);
    event RootPublished(
        uint64 indexed period,
        bytes32 inputsHash,
        bytes32 payoutRoot,
        bytes32 billRoot,
        uint256 totalCumPayout,
        uint256 totalCumBill
    );
    event RootVoided(uint64 indexed period, address by);
    event RootsDropped(uint64 fromPeriod, uint64 toPeriod);
    event SignerSetUpdated(address[] signers, uint8 threshold);
    event PriceSet(uint32 indexed version, bytes32 indexed tier, uint256 usdcPerDay);
    event MinSignerBondSet(uint256 amount);
    event RegistrarSet(address registrar);
    event BondDeposited(address indexed who, uint256 amount);
    event BondSlashed(address indexed who, uint256 amount, address to);
    event BondReleased(address indexed who, uint256 amount);

    error ZeroAddress();
    error AccountExists();
    error NoAccount();
    error NotAccountKey();
    error NothingPending();
    error TooEarly();
    error BadPeriod();
    error UnknownPriceTable();
    error PayoutExceedsBill();
    error BillExceedsDeposits();
    error NotMonotonic();
    error BadSignatures();
    error NotSigner();
    error WindowClosed();
    error AlreadyVoided();
    error NoFinalRoot();
    error BadProof();
    error NothingToClaim();
    error BadSignerSet();
    error BadRegistrarSig();

    constructor(IERC20 usdc_, address governance) Ownable(governance) {
        usdc = usdc_;
    }

    // ---- accounts -----------------------------------------------------------------------------

    /// @notice Creates account `id` for `key`. `registrarSig` is the registrar's EIP-191 signature over
    ///         `keccak256(abi.encode(block.chainid, address(this), id, key))`.
    function createAccount(bytes32 id, address key, bytes calldata registrarSig) external {
        if (key == address(0)) revert ZeroAddress();
        if (accounts[id].key != address(0)) revert AccountExists();
        bytes32 h =
            MessageHashUtils.toEthSignedMessageHash(keccak256(abi.encode(block.chainid, address(this), id, key)));
        if (ECDSA.recover(h, registrarSig) != registrar) revert BadRegistrarSig();
        accounts[id].key = key;
        emit AccountCreated(id, key);
    }

    function deposit(bytes32 id, uint256 amount) external {
        Account storage a = accounts[id];
        if (a.key == address(0)) revert NoAccount();
        a.balance += amount;
        totalDeposited += amount;
        usdc.safeTransferFrom(msg.sender, address(this), amount);
        emit Deposited(id, msg.sender, amount);
    }

    /// Off-chain rule: resolvers stop serving an account whose balance minus pending withdrawal cannot
    /// cover four days at list price, so a customer cannot be served on money that is on its way out.
    function requestWithdraw(bytes32 id, uint256 amount) external {
        Account storage a = accounts[id];
        if (msg.sender != a.key) revert NotAccountKey();
        a.pendingWithdrawal = amount;
        a.withdrawableAt = uint64(block.timestamp) + WITHDRAWAL_DELAY;
        emit WithdrawRequested(id, amount, a.withdrawableAt);
    }

    function cancelWithdraw(bytes32 id) external {
        Account storage a = accounts[id];
        if (msg.sender != a.key) revert NotAccountKey();
        a.pendingWithdrawal = 0;
        emit WithdrawCancelled(id);
    }

    /// Applies the account's cumulative bill from the latest final root, then pays the pending amount,
    /// capped by what is left. `cumBill` and `proof` come from the published leaf list of that root.
    function executeWithdraw(bytes32 id, uint256 cumBill, bytes32[] calldata proof) external {
        Account storage a = accounts[id];
        if (a.pendingWithdrawal == 0) revert NothingPending();
        if (block.timestamp < a.withdrawableAt) revert TooEarly();
        uint256 billApplied = _applyBill(id, a, cumBill, proof);
        uint256 amount = a.pendingWithdrawal;
        if (amount > a.balance) amount = a.balance;
        a.pendingWithdrawal = 0;
        a.balance -= amount;
        totalWithdrawn += amount;
        usdc.safeTransfer(a.key, amount);
        emit Withdrawn(id, amount, billApplied);
    }

    function _applyBill(bytes32 id, Account storage a, uint256 cumBill, bytes32[] calldata proof)
        internal
        returns (uint256 applied)
    {
        uint64 p = latestFinalPeriod();
        if (p == 0) return 0; // no bill has ever been published
        bytes32 leaf = keccak256(bytes.concat(keccak256(abi.encode(id, cumBill))));
        if (!MerkleProof.verify(proof, roots[p].billRoot, leaf)) revert BadProof();
        if (cumBill < a.appliedBill) revert NotMonotonic();
        applied = cumBill - a.appliedBill;
        a.appliedBill = cumBill;
        a.balance = applied >= a.balance ? 0 : a.balance - applied;
    }

    // ---- payouts ------------------------------------------------------------------------------

    /// Pays `cumPayout - claimed[payee]` against the latest final payout root. Anyone may call.
    function claim(address payee, uint256 cumPayout, bytes32[] calldata proof) external {
        uint64 p = latestFinalPeriod();
        if (p == 0) revert NoFinalRoot();
        bytes32 leaf = keccak256(bytes.concat(keccak256(abi.encode(payee, cumPayout))));
        if (!MerkleProof.verify(proof, roots[p].payoutRoot, leaf)) revert BadProof();
        uint256 already = claimed[payee];
        if (cumPayout <= already) revert NothingToClaim();
        claimed[payee] = cumPayout;
        usdc.safeTransfer(payee, cumPayout - already);
        emit Claimed(payee, cumPayout - already, cumPayout);
    }

    // ---- roots --------------------------------------------------------------------------------

    /// One call per period, `threshold` distinct bonded signers over `digest(r)`. A voided period is republished
    /// under the same number before the next one. Totals are cumulative and can never decrease; payouts never
    /// exceed bills and bills never exceed what customers have deposited and not withdrawn.
    function publishRoot(RootInput calldata r, bytes[] calldata sigs) external {
        // only the latest period can be voided (voiding an earlier one drops the later ones)
        if (r.period != (roots[lastPeriod].voided ? lastPeriod : lastPeriod + 1)) revert BadPeriod();
        if (!priceVersionSet[r.priceTableVersion]) revert UnknownPriceTable();
        if (r.totalCumPayout > r.totalCumBill) revert PayoutExceedsBill();
        if (r.totalCumBill > totalDeposited - totalWithdrawn) revert BillExceedsDeposits();
        Root storage prev = roots[r.period - 1];
        if (r.totalCumPayout < prev.totalCumPayout || r.totalCumBill < prev.totalCumBill) revert NotMonotonic();
        _checkSignatures(digest(r), sigs);
        roots[r.period] = Root(
            r.inputsHash,
            r.payoutRoot,
            r.billRoot,
            r.totalCumPayout,
            r.totalCumBill,
            r.registryBlock,
            r.priceTableVersion,
            uint64(block.timestamp),
            false
        );
        lastPeriod = r.period;
        emit RootPublished(r.period, r.inputsHash, r.payoutRoot, r.billRoot, r.totalCumPayout, r.totalCumBill);
    }

    function digest(RootInput calldata r) public view returns (bytes32) {
        return keccak256(
            abi.encode(
                block.chainid,
                address(this),
                r.period,
                r.inputsHash,
                r.registryBlock,
                r.priceTableVersion,
                r.payoutRoot,
                r.billRoot,
                r.totalCumPayout,
                r.totalCumBill
            )
        );
    }

    function _checkSignatures(bytes32 d, bytes[] calldata sigs) internal view {
        if (threshold == 0) revert BadSignatures();
        bytes32 h = MessageHashUtils.toEthSignedMessageHash(d);
        address[] memory seen = new address[](sigs.length);
        uint256 valid;
        for (uint256 i = 0; i < sigs.length; i++) {
            address s = ECDSA.recover(h, sigs[i]);
            if (!isSigner[s]) revert BadSignatures();
            for (uint256 j = 0; j < i; j++) {
                if (seen[j] == s) revert BadSignatures();
            }
            seen[i] = s;
            if (bonds[s] >= minSignerBond) valid++; // an unbonded signer's signature is ignored
        }
        if (valid < threshold) revert BadSignatures();
    }

    /// @notice Any signer or governance can void a root while its own window is open. The roots published after
    ///         it were computed on top of it and are dropped with it: `lastPeriod` rolls back to `period`, which
    ///         has to be republished before the next one.
    function voidRoot(uint64 period) external {
        if (!isSigner[msg.sender] && msg.sender != owner()) revert NotSigner();
        Root storage rt = roots[period];
        if (period == 0 || period > lastPeriod) revert BadPeriod();
        if (rt.voided) revert AlreadyVoided();
        if (block.timestamp >= rt.publishedAt + DISPUTE_WINDOW) revert WindowClosed();
        rt.voided = true;
        emit RootVoided(period, msg.sender);
        if (period < lastPeriod) {
            emit RootsDropped(period + 1, lastPeriod);
            lastPeriod = period;
        }
    }

    function isFinal(uint64 period) public view returns (bool) {
        Root storage rt = roots[period];
        return
            period <= lastPeriod && rt.publishedAt != 0 && !rt.voided
                && block.timestamp >= rt.publishedAt + DISPUTE_WINDOW;
    }

    function latestFinalPeriod() public view returns (uint64 p) {
        p = lastPeriod;
        while (p > 0 && !isFinal(p)) p--;
    }

    // ---- governance ---------------------------------------------------------------------------

    function setSignerSet(address[] calldata newSigners, uint8 newThreshold) external onlyOwner {
        for (uint256 i = 0; i < signers.length; i++) {
            isSigner[signers[i]] = false;
        }
        delete signers;
        for (uint256 i = 0; i < newSigners.length; i++) {
            if (newSigners[i] == address(0) || isSigner[newSigners[i]]) revert BadSignerSet();
            isSigner[newSigners[i]] = true;
            signers.push(newSigners[i]);
        }
        if (newThreshold == 0 || newThreshold > newSigners.length) revert BadSignerSet();
        threshold = newThreshold;
        emit SignerSetUpdated(newSigners, newThreshold);
    }

    function signerCount() external view returns (uint256) {
        return signers.length;
    }

    /// @notice Sets the list price of a tier in a price table version; a root may name a version once it has one.
    function setPrice(uint32 version, bytes32 tier, uint256 usdcPerDay) external onlyOwner {
        prices[version][tier] = usdcPerDay;
        priceVersionSet[version] = true;
        emit PriceSet(version, tier, usdcPerDay);
    }

    /// @notice The tier key of `prices`: node count, bandwidth class in Mbps, availability in basis points
    ///         (6 nodes, 100 Mbps, 99.9 % → `tierKey(6, 100, 9990)`).
    function tierKey(uint16 nodes, uint32 classMbps, uint32 availabilityBps) external pure returns (bytes32) {
        return keccak256(abi.encode(nodes, classMbps, availabilityBps));
    }

    function setMinSignerBond(uint256 amount) external onlyOwner {
        minSignerBond = amount;
        emit MinSignerBondSet(amount);
    }

    function setRegistrar(address registrar_) external onlyOwner {
        registrar = registrar_;
        emit RegistrarSet(registrar_);
    }

    // Bonds of the aggregator and countersigner. ponytail: release is governance-gated in v1; a timed
    // unbond like the Registry's replaces it once signers rotate on their own.
    function depositBond(uint256 amount) external {
        bonds[msg.sender] += amount;
        usdc.safeTransferFrom(msg.sender, address(this), amount);
        emit BondDeposited(msg.sender, amount);
    }

    function slashBond(address who, uint256 amount, address to) external onlyOwner {
        if (amount > bonds[who]) amount = bonds[who];
        bonds[who] -= amount;
        usdc.safeTransfer(to, amount);
        emit BondSlashed(who, amount, to);
    }

    function releaseBond(address who) external onlyOwner {
        uint256 amount = bonds[who];
        bonds[who] = 0;
        usdc.safeTransfer(who, amount);
        emit BondReleased(who, amount);
    }
}
