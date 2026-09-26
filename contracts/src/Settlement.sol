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
    address[] public signers;
    mapping(address => bool) public isSigner;
    uint8 public threshold;
    mapping(uint32 version => bytes32) public priceTable;
    mapping(address => uint256) public bonds;

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
    event SignerSetUpdated(address[] signers, uint8 threshold);
    event PriceTableSet(uint32 indexed version, bytes32 hash);
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

    constructor(IERC20 usdc_, address governance) Ownable(governance) {
        usdc = usdc_;
    }

    // ---- accounts -----------------------------------------------------------------------------

    function createAccount(bytes32 id, address key) external {
        if (key == address(0)) revert ZeroAddress();
        if (accounts[id].key != address(0)) revert AccountExists();
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

    /// One call per period, `threshold` distinct signers over `digest(r)`. A voided period is republished
    /// under the same number. Totals are cumulative and can never decrease; payouts never exceed bills and
    /// bills never exceed what customers have deposited in total.
    function publishRoot(RootInput calldata r, bytes[] calldata sigs) external {
        if (r.period == 0) revert BadPeriod();
        bool republish = r.period == lastPeriod && roots[lastPeriod].voided;
        if (!republish && r.period != lastPeriod + 1) revert BadPeriod();
        if (priceTable[r.priceTableVersion] == bytes32(0)) revert UnknownPriceTable();
        if (r.totalCumPayout > r.totalCumBill) revert PayoutExceedsBill();
        if (r.totalCumBill > totalDeposited) revert BillExceedsDeposits();
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
            for (uint256 j = 0; j < valid; j++) {
                if (seen[j] == s) revert BadSignatures();
            }
            seen[valid++] = s;
        }
        if (valid < threshold) revert BadSignatures();
    }

    /// Any signer or governance can void the latest root while its window is open.
    function voidRoot(uint64 period) external {
        if (!isSigner[msg.sender] && msg.sender != owner()) revert NotSigner();
        Root storage rt = roots[period];
        if (period == 0 || period != lastPeriod || rt.publishedAt == 0) revert BadPeriod();
        if (rt.voided) revert AlreadyVoided();
        if (block.timestamp >= rt.publishedAt + DISPUTE_WINDOW) revert WindowClosed();
        rt.voided = true;
        emit RootVoided(period, msg.sender);
    }

    function isFinal(uint64 period) public view returns (bool) {
        Root storage rt = roots[period];
        return rt.publishedAt != 0 && !rt.voided && block.timestamp >= rt.publishedAt + DISPUTE_WINDOW;
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

    function setPriceTable(uint32 version, bytes32 hash) external onlyOwner {
        priceTable[version] = hash;
        emit PriceTableSet(version, hash);
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
