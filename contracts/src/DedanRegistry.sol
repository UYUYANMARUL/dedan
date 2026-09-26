// SPDX-License-Identifier: MIT
pragma solidity 0.8.25;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {ERC1155Singleton} from "@ensdomains/contracts-v2/erc1155/ERC1155Singleton.sol";
import {PermissionedRegistry} from "@ensdomains/contracts-v2/registry/PermissionedRegistry.sol";
import {IRegistry} from "@ensdomains/contracts-v2/registry/interfaces/IRegistry.sol";
import {ILabelStore} from "@ensdomains/contracts-v2/utils/interfaces/ILabelStore.sol";
import {LibLabel} from "@ensdomains/contracts-v2/utils/LibLabel.sol";
import {NameCoder} from "@ens/contracts/utils/NameCoder.sol";
import {DedanResolver} from "./DedanResolver.sol";
import {DedanRoles} from "./DedanRoles.sol";
import {IWorldIDVerifier} from "./IWorldIDVerifier.sol";
import {LibCell} from "./LibCell.sol";

/// @title Dedan's identity layer: the `dedan.eth` subname registry (ENSv2) with Dedan's own rules.
/// @notice Nodes are subname tokens; their endpoint and config are ENS records the owner writes on the
///         resolver; every permission is an Enhanced Access Control role on this contract. Verified humans
///         (World ID) get the roles that let them register nodes, attest round trips or run assigned probes.
///         Cells (the regions of the latency map, `DedanCells`) are names here too, with this registry as their
///         subregistry, so every node name also resolves under every cell: `<label>.<cell>.dedan.eth`.
/// @dev Roles and the reason for the per-human resource are in `DedanRoles`. Stakes are USDC.
contract DedanRegistry is PermissionedRegistry {
    using SafeERC20 for IERC20;

    enum StakeKind {
        Node,
        Auditor,
        Resolver
    }

    struct WorldIdProof {
        uint256 nullifier;
        uint256 nonce;
        uint64 expiresAtMin;
        uint64 issuerSchemaId;
        uint256 credentialGenesisIssuedAtMin;
        uint256[5] proof;
    }

    uint64 public constant UNBONDING_PERIOD = 7 days;
    uint256 public constant ACTION_OPERATOR = uint256(keccak256("dedan-operator")) >> 8;
    uint256 public constant ACTION_ATTESTER = uint256(keccak256("dedan-attester")) >> 8;
    uint256 public constant ACTION_AUDITOR = uint256(keccak256("dedan-auditor")) >> 8;

    IERC20 public immutable USDC;
    /// @notice namehash of the parent name, e.g. `dedan.eth`.
    bytes32 public immutable PARENT_NODE;
    IWorldIDVerifier public worldId;
    uint64 public worldIdRpId;
    DedanResolver public dedanResolver;
    /// @notice The cells contract: registers cell names here and resolves them.
    address public cells;

    mapping(uint256 action => mapping(uint256 nullifier => bool used)) public nullifierUsed;
    mapping(StakeKind kind => uint256 amount) public minStake;
    mapping(uint256 labelId => uint256 amount) public nodeStake;
    mapping(uint256 labelId => uint64 at) public nodeUnbondAt;
    mapping(uint256 labelId => bool suspended) public nodeSuspended;
    mapping(address account => uint256 amount) public accountStake;
    mapping(address account => uint64 at) public accountUnbondAt;
    mapping(address account => bool suspended) public accountSuspended;

    event HumanVerified(address indexed account, uint256 indexed action, uint256 nullifier);
    event NodeRegistered(bytes32 indexed labelHash, string label, address indexed owner, uint256 stake);
    event NodeUnbonding(bytes32 indexed labelHash, uint64 unbondAt);
    event NodeWithdrawn(bytes32 indexed labelHash, address indexed to, uint256 amount);
    event NodeSuspended(bytes32 indexed labelHash, bool suspended);
    event NodeSlashed(bytes32 indexed labelHash, uint256 amount, address to);
    event AccountStaked(address indexed account, uint256 amount);
    event AccountUnbonding(address indexed account, uint64 unbondAt);
    event AccountWithdrawn(address indexed account, uint256 amount);
    event AccountSuspended(address indexed account, bool suspended);
    event AccountSlashed(address indexed account, uint256 amount, address to);
    event KeyRevealed(bytes32 indexed labelHash, uint64 indexed hour, bytes32 kHour);
    event MinStakeSet(StakeKind kind, uint256 amount);
    event WorldIdSet(address verifier, uint64 rpId);
    event ResolverSet(address resolver);
    event CellsSet(address cells);

    error UseRegisterNode();
    error NotNodeOwner();
    error StakeTooLow();
    error NullifierUsed();
    error StillUnbonding();
    error WrongStatus();
    error AccountIsSuspended();
    error ZeroAddress();
    error NotCells();
    error CellLabel();

    constructor(
        ILabelStore labelStore,
        address governance,
        IERC20 usdc,
        IWorldIDVerifier worldId_,
        uint64 rpId,
        string memory parentName
    ) PermissionedRegistry(labelStore, governance, DedanRoles.GOVERNANCE_BITMAP) {
        USDC = usdc;
        worldId = worldId_;
        worldIdRpId = rpId;
        PARENT_NODE = NameCoder.namehash(NameCoder.encode(parentName), 0);
    }

    // ---- governance ---------------------------------------------------------------------------

    function setDedanResolver(DedanResolver resolver) external onlyRootRoles(DedanRoles.ROLE_CONFIGURE) {
        if (address(resolver) == address(0)) revert ZeroAddress();
        dedanResolver = resolver;
        emit ResolverSet(address(resolver));
    }

    /// @notice The cells contract, also told to the node resolver so `<label>.<cell>` alias names resolve.
    function setCells(address cells_) external onlyRootRoles(DedanRoles.ROLE_CONFIGURE) {
        if (cells_ == address(0)) revert ZeroAddress();
        cells = cells_;
        dedanResolver.setCells(cells_);
        emit CellsSet(cells_);
    }

    function setMinStake(StakeKind kind, uint256 amount) external onlyRootRoles(DedanRoles.ROLE_CONFIGURE) {
        minStake[kind] = amount;
        emit MinStakeSet(kind, amount);
    }

    function setWorldId(IWorldIDVerifier verifier, uint64 rpId) external onlyRootRoles(DedanRoles.ROLE_CONFIGURE) {
        worldId = verifier;
        worldIdRpId = rpId;
        emit WorldIdSet(address(verifier), rpId);
    }

    function suspendNode(string calldata label, bool suspended) external onlyRootRoles(DedanRoles.ROLE_SUSPEND) {
        uint256 labelId = LibLabel.id(label);
        nodeSuspended[labelId] = suspended;
        emit NodeSuspended(bytes32(labelId), suspended);
    }

    function suspendAccount(address account, bool suspended) external onlyRootRoles(DedanRoles.ROLE_SUSPEND) {
        accountSuspended[account] = suspended;
        emit AccountSuspended(account, suspended);
    }

    function slashNode(string calldata label, uint256 amount, address to)
        external
        onlyRootRoles(DedanRoles.ROLE_SLASH)
    {
        uint256 labelId = LibLabel.id(label);
        if (amount > nodeStake[labelId]) amount = nodeStake[labelId];
        nodeStake[labelId] -= amount;
        USDC.safeTransfer(to, amount);
        emit NodeSlashed(bytes32(labelId), amount, to);
    }

    function slashAccount(address account, uint256 amount, address to) external onlyRootRoles(DedanRoles.ROLE_SLASH) {
        if (amount > accountStake[account]) amount = accountStake[account];
        accountStake[account] -= amount;
        USDC.safeTransfer(to, amount);
        emit AccountSlashed(account, amount, to);
    }

    // ---- verified humans ----------------------------------------------------------------------

    /// @notice One human, one operator account. The operator then registers as many nodes as it stakes for.
    function registerOperator(WorldIdProof calldata p) external {
        _verifyHuman(ACTION_OPERATOR, p);
        _grantRoles(_humanResource(msg.sender), DedanRoles.ROLE_NODE_OPERATOR, msg.sender, false);
    }

    /// @notice One human, one attester account; no stake, paid per valid attestation.
    function registerAttester(WorldIdProof calldata p) external {
        _verifyHuman(ACTION_ATTESTER, p);
        _grantRoles(_humanResource(msg.sender), DedanRoles.ROLE_ATTESTER, msg.sender, false);
    }

    /// @notice One human, one auditor account, with the auditor stake; may run assigned probes.
    function registerAuditor(WorldIdProof calldata p, uint256 stake) external {
        if (stake < minStake[StakeKind.Auditor]) revert StakeTooLow();
        _verifyHuman(ACTION_AUDITOR, p);
        _addAccountStake(stake);
        _grantRoles(_humanResource(msg.sender), DedanRoles.ROLE_AUDITOR, msg.sender, false);
    }

    /// @notice Stake for an account role (auditor top-up, resolver operator bond).
    function stakeAccount(uint256 amount) external {
        _addAccountStake(amount);
    }

    function requestUnbondAccount() external {
        if (accountUnbondAt[msg.sender] != 0) revert WrongStatus();
        uint64 at = uint64(block.timestamp) + UNBONDING_PERIOD;
        accountUnbondAt[msg.sender] = at;
        emit AccountUnbonding(msg.sender, at);
    }

    function withdrawAccount() external {
        uint64 at = accountUnbondAt[msg.sender];
        if (at == 0) revert WrongStatus();
        if (block.timestamp < at) revert StillUnbonding();
        uint256 amount = accountStake[msg.sender];
        accountStake[msg.sender] = 0;
        accountUnbondAt[msg.sender] = 0;
        USDC.safeTransfer(msg.sender, amount);
        emit AccountWithdrawn(msg.sender, amount);
    }

    // ---- nodes --------------------------------------------------------------------------------

    /// @notice A verified operator registers a node: the name is minted to it with the operator roles, the
    ///         hot keys get `ROLE_NODE_SIGNER`, the stake is held here. The operator then writes the node's
    ///         records (`addr`, `dedan.*` text, DNS A/AAAA) on the resolver, as any ENS owner does.
    function registerNode(string calldata label, address[] calldata signers, uint256 stake)
        external
        returns (uint256 tokenId)
    {
        _requireHuman(DedanRoles.ROLE_NODE_OPERATOR);
        if (stake < minStake[StakeKind.Node]) revert StakeTooLow();
        if (address(dedanResolver) == address(0)) revert ZeroAddress();
        if (LibCell.isCellLabel(label)) revert CellLabel();
        uint256 labelId = LibLabel.id(label);
        tokenId = _register(
            label,
            msg.sender,
            IRegistry(address(0)),
            address(dedanResolver),
            DedanRoles.OPERATOR_BITMAP,
            type(uint64).max,
            false
        );
        nodeStake[labelId] = stake;
        USDC.safeTransferFrom(msg.sender, address(this), stake);
        dedanResolver.bind(nodeOf(label), labelId);
        uint256 resource = getResource(labelId);
        for (uint256 i; i < signers.length; ++i) {
            _grantRoles(resource, DedanRoles.ROLE_NODE_SIGNER, signers[i], false);
        }
        emit NodeRegistered(bytes32(labelId), label, msg.sender, stake);
    }

    function requestUnbond(string calldata label) external onlyNodeOwner(label) {
        uint256 labelId = LibLabel.id(label);
        if (nodeUnbondAt[labelId] != 0) revert WrongStatus();
        uint64 at = uint64(block.timestamp) + UNBONDING_PERIOD;
        nodeUnbondAt[labelId] = at;
        emit NodeUnbonding(bytes32(labelId), at);
    }

    /// @notice After unbonding: the stake comes back to the current owner and the name is removed, which
    ///         bumps its EAC version so no old role survives a later re-registration.
    function withdraw(string calldata label) external onlyNodeOwner(label) {
        uint256 labelId = LibLabel.id(label);
        uint64 at = nodeUnbondAt[labelId];
        if (at == 0) revert WrongStatus();
        if (block.timestamp < at) revert StillUnbonding();
        uint256 amount = nodeStake[labelId];
        nodeStake[labelId] = 0;
        nodeUnbondAt[labelId] = 0;
        nodeSuspended[labelId] = false;
        _removeNode(labelId);
        USDC.safeTransfer(msg.sender, amount);
        emit NodeWithdrawn(bytes32(labelId), msg.sender, amount);
    }

    /// @notice A node's hot key reveals the key that minted the hour's fetch tokens (ARCHITECTURE.md §7).
    function revealKey(string calldata label, uint64 hour, bytes32 kHour) external {
        uint256 labelId = LibLabel.id(label);
        _checkRoles(labelId, DedanRoles.ROLE_NODE_SIGNER, msg.sender);
        emit KeyRevealed(bytes32(labelId), hour, kHour);
    }

    // ---- cells --------------------------------------------------------------------------------

    /// @notice The cells contract registers a cell's name on its first member: owned and resolved by the cells
    ///         contract, with this registry as the subregistry so every node name resolves under the cell too.
    function registerCell(uint64 cell) external returns (bytes32 node) {
        if (msg.sender != cells) revert NotCells();
        string memory label = LibCell.label(cell);
        _register(label, cells, IRegistry(address(this)), cells, 0, type(uint64).max, false);
        return nodeOf(label);
    }

    /// @notice namehash of `<cell>.<parent>`.
    function cellNode(uint64 cell) external view returns (bytes32) {
        return nodeOf(LibCell.label(cell));
    }

    // ---- views --------------------------------------------------------------------------------

    /// @notice namehash of `<label>.<parent>`.
    function nodeOf(string memory label) public view returns (bytes32) {
        return NameCoder.namehash(PARENT_NODE, bytes32(LibLabel.id(label)));
    }

    function nodeOwner(string calldata label) external view returns (address) {
        return getOwner(LibLabel.id(label));
    }

    function isNodeActive(string calldata label) external view returns (bool) {
        uint256 labelId = LibLabel.id(label);
        return getOwner(labelId) != address(0) && !nodeSuspended[labelId] && nodeUnbondAt[labelId] == 0;
    }

    function isNodeSigner(string calldata label, address key) external view returns (bool) {
        return hasRoles(LibLabel.id(label), DedanRoles.ROLE_NODE_SIGNER, key);
    }

    function isOperator(address account) public view returns (bool) {
        return _hasHumanRole(account, DedanRoles.ROLE_NODE_OPERATOR);
    }

    function isAttester(address account) public view returns (bool) {
        return _hasHumanRole(account, DedanRoles.ROLE_ATTESTER) && !accountSuspended[account];
    }

    /// @notice An auditor may run assigned probes while it holds the governance minimum stake.
    function isAuditor(address account) public view returns (bool) {
        return _hasHumanRole(account, DedanRoles.ROLE_AUDITOR) && !accountSuspended[account]
            && accountStake[account] >= minStake[StakeKind.Auditor];
    }

    function isResolverOperator(address account) external view returns (bool) {
        return hasRootRoles(DedanRoles.ROLE_RESOLVER_OPERATOR, account) && !accountSuspended[account];
    }

    /// @notice World ID signal: the account, hashed to the field as IDKit does.
    function signalHash(address account) public pure returns (uint256) {
        return uint256(keccak256(abi.encodePacked(account))) >> 8;
    }

    /// @dev The only door into this registry is `registerNode`.
    function register(string memory, address, IRegistry, address, uint256, uint64)
        public
        pure
        override
        returns (uint256)
    {
        revert UseRegisterNode();
    }

    // ---- internals ----------------------------------------------------------------------------

    modifier onlyNodeOwner(string calldata label) {
        if (getOwner(LibLabel.id(label)) != msg.sender) revert NotNodeOwner();
        _;
    }

    /// @dev The registry's role checks translate any id through `getResource`; a human's resource never has a
    ///      registry entry, so the translation is stable, and grants, checks and views all go through it.
    function _humanResource(address account) internal view returns (uint256) {
        return getResource(DedanRoles.humanResource(account));
    }

    function _requireHuman(uint256 role) internal view {
        if (accountSuspended[msg.sender]) revert AccountIsSuspended();
        _checkRoles(_humanResource(msg.sender), role, msg.sender);
    }

    function _hasHumanRole(address account, uint256 role) internal view returns (bool) {
        return _getRoles(_humanResource(account), account) & role != 0;
    }

    function _verifyHuman(uint256 action, WorldIdProof calldata p) internal {
        if (nullifierUsed[action][p.nullifier]) revert NullifierUsed();
        worldId.verify(
            p.nullifier,
            action,
            worldIdRpId,
            p.nonce,
            signalHash(msg.sender),
            p.expiresAtMin,
            p.issuerSchemaId,
            p.credentialGenesisIssuedAtMin,
            p.proof
        );
        nullifierUsed[action][p.nullifier] = true;
        emit HumanVerified(msg.sender, action, p.nullifier);
    }

    function _addAccountStake(uint256 amount) internal {
        accountStake[msg.sender] += amount;
        USDC.safeTransferFrom(msg.sender, address(this), amount);
        emit AccountStaked(msg.sender, amount);
    }

    /// @dev Same as `unregister`, without the role check: the owner already proved itself and the stake is gone.
    function _removeNode(uint256 labelId) internal {
        Entry storage entry = _entry(labelId);
        uint256 tokenId = _constructTokenId(labelId, entry);
        address owner = ERC1155Singleton.ownerOf(tokenId);
        emit LabelUnregistered(tokenId, msg.sender);
        if (owner != address(0)) {
            _burn(owner, tokenId, 1);
            ++entry.eacVersionId;
            ++entry.tokenVersionId;
        }
        entry.expiry = uint64(block.timestamp);
    }
}
