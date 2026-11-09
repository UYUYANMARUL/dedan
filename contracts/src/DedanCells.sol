// SPDX-License-Identifier: MIT
pragma solidity 0.8.25;

import {IERC1155Receiver} from "@openzeppelin/contracts/token/ERC1155/IERC1155Receiver.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";
import {Strings} from "@openzeppelin/contracts/utils/Strings.sol";
import {LibLabel} from "@ensdomains/contracts-v2/utils/LibLabel.sol";
import {ITextResolver} from "@ens/contracts/resolvers/profiles/ITextResolver.sol";
import {IDNSRecordResolver} from "@ens/contracts/resolvers/profiles/IDNSRecordResolver.sol";
import {RRUtils} from "@ens/contracts/dnssec-oracle/RRUtils.sol";
import {BytesUtils} from "@ens/contracts/utils/BytesUtils.sol";
import {NameCoder} from "@ens/contracts/utils/NameCoder.sol";
import {DedanRegistry} from "./DedanRegistry.sol";
import {LibCell} from "./LibCell.sol";

/// @title Dedan's cells: the regions of the latency map as ENS names, node membership, and the median on chain.
/// @notice A cell is an H3 resolution-2 index (about 300 km across). Its first member registers it as
///         `<hex>.dedan.eth` with this contract as resolver and the Dedan registry as subregistry, so every node
///         name also resolves as `<label>.<cell>.dedan.eth`. Nodes join the cells they serve. Verified humans
///         attest round trips to a cell's members, one batch per minute per cell; each human holds one vote per
///         node per cell (its newest sample), votes land in a per-day histogram of 8 ms buckets, and the median over
///         today and yesterday is the cell's record for that node (ARCHITECTURE.md §6, §7). The cell name's A/AAAA
///         records are its members' own, lowest median first: what a Dedan nameserver answers a client in the cell.
/// @dev Nothing here is trusted by money directly: the aggregator recomputes pay and evidence from the `Attested`
///      events; the histograms are what resolvers steer by, and any ENS client can read them as text records.
contract DedanCells is ITextResolver, IDNSRecordResolver, IERC1155Receiver {
    struct Sample {
        bytes32 labelHash;
        uint32 rttMs;
        uint32 hitTtfbMs;
        bytes token;
        bytes prefix;
    }

    struct Vote {
        uint32 epoch;
        uint8 bucket;
        bool set;
    }

    struct Alias {
        uint64 cell;
        uint256 labelId;
    }

    uint256 public constant BUCKETS = 32;
    uint32 public constant BUCKET_MS = 8;
    uint64 public constant EPOCH = 1 days;
    uint64 public constant MIN_INTERVAL = 60;

    DedanRegistry public immutable REGISTRY;
    /// @notice Votes a member needs before the cell's DNS records name it: 3 in production, 1 on the devnet.
    uint32 public immutable MIN_VOTES;
    /// @dev The parent name in DNS wire form (`\x05dedan\x03eth\x00`), to name the cell and its members.
    bytes internal _parentDns;

    mapping(uint64 cell => uint256[] labelIds) internal _members;
    mapping(uint64 cell => mapping(uint256 labelId => uint32 index1)) internal _memberIndex;
    /// @dev The cells a node is a member of, so the registry can remove it from all of them.
    mapping(uint256 labelId => uint64[] cells) internal _cellsOf;
    /// @dev Two words of sixteen 16-bit counters: bucket b lives in word b / 16 at bit (b % 16) * 16.
    mapping(uint64 cell => mapping(uint256 labelId => mapping(uint32 epoch => uint256[2]))) internal _hist;
    mapping(address human => mapping(uint64 cell => mapping(uint256 labelId => Vote))) public lastVote;
    mapping(address human => mapping(uint64 cell => uint64 at)) public lastBatchAt;
    mapping(uint64 cell => bool) public registered;
    /// @notice namehash of `<cell>.dedan.eth` → cell.
    mapping(bytes32 node => uint64 cell) public cellOfNode;
    /// @notice namehash of `<label>.<cell>.dedan.eth` → (cell, label id), set while the node is a member.
    mapping(bytes32 node => Alias) public aliasOf;

    event Joined(uint64 indexed cell, bytes32 indexed labelHash);
    event Left(uint64 indexed cell, bytes32 indexed labelHash);
    event Attested(
        address indexed attester,
        uint64 indexed cell,
        bytes32 indexed labelHash,
        uint32 rttMs,
        uint32 hitTtfbMs,
        bytes token,
        bytes prefix,
        uint64 ts
    );

    error NotNodeOwner();
    error NodeInactive();
    error NotAttester();
    error NotMember();
    error TooSoon();
    error NotRegistry();
    error BadCell(uint64 cell);

    constructor(DedanRegistry registry, string memory parentName, uint32 minVotes) {
        REGISTRY = registry;
        _parentDns = NameCoder.encode(parentName);
        MIN_VOTES = minVotes;
    }

    // ---- membership ---------------------------------------------------------------------------

    /// @notice A node's owner adds it to the cells it serves; the first member of a cell registers the cell name.
    ///         Cells are H3 resolution-2 indexes (`LibCell.isCell`).
    function join(string calldata label, uint64[] calldata cells) external {
        uint256 labelId = LibLabel.id(label);
        if (REGISTRY.getOwner(labelId) != msg.sender) revert NotNodeOwner();
        if (!REGISTRY.isNodeActive(label)) revert NodeInactive();
        for (uint256 i; i < cells.length; ++i) {
            uint64 cell = cells[i];
            if (!LibCell.isCell(cell)) revert BadCell(cell);
            if (!registered[cell]) {
                registered[cell] = true;
                cellOfNode[REGISTRY.registerCell(cell)] = cell;
            }
            if (_memberIndex[cell][labelId] != 0) continue;
            _members[cell].push(labelId);
            _memberIndex[cell][labelId] = uint32(_members[cell].length);
            _cellsOf[labelId].push(cell);
            aliasOf[aliasNode(cell, labelId)] = Alias(cell, labelId);
            emit Joined(cell, bytes32(labelId));
        }
    }

    function leave(string calldata label, uint64[] calldata cells) external {
        uint256 labelId = LibLabel.id(label);
        if (REGISTRY.getOwner(labelId) != msg.sender) revert NotNodeOwner();
        uint64[] storage mine = _cellsOf[labelId];
        for (uint256 i; i < cells.length; ++i) {
            uint64 cell = cells[i];
            if (_memberIndex[cell][labelId] == 0) continue;
            _drop(cell, labelId);
            for (uint256 j;; ++j) {
                if (mine[j] == cell) {
                    mine[j] = mine[mine.length - 1];
                    mine.pop();
                    break;
                }
            }
        }
    }

    /// @notice Called by the registry when a node is withdrawn: it leaves every cell it is a member of.
    function removeNode(uint256 labelId) external {
        if (msg.sender != address(REGISTRY)) revert NotRegistry();
        uint64[] storage mine = _cellsOf[labelId];
        for (uint256 i; i < mine.length; ++i) {
            _drop(mine[i], labelId);
        }
        delete _cellsOf[labelId];
    }

    // ---- attestations -------------------------------------------------------------------------

    /// @notice A verified human records round trips to a cell's members from inside that cell. One batch per
    ///         minute per cell; one vote per node per cell, the newest sample replacing the previous one.
    function attest(uint64 cell, Sample[] calldata batch) external {
        if (!REGISTRY.isAttester(msg.sender)) revert NotAttester();
        uint64 ts = uint64(block.timestamp);
        if (lastBatchAt[msg.sender][cell] + MIN_INTERVAL > ts) revert TooSoon();
        lastBatchAt[msg.sender][cell] = ts;
        for (uint256 i; i < batch.length; ++i) {
            _vote(cell, uint256(batch[i].labelHash), batch[i].rttMs, uint32(ts / EPOCH));
            _record(cell, batch[i], ts);
        }
    }

    /// @dev The human's vote for the node in the cell: the previous one, if inside the window, is withdrawn first.
    function _vote(uint64 cell, uint256 labelId, uint32 rttMs, uint32 epoch) internal {
        if (_memberIndex[cell][labelId] == 0) revert NotMember();
        uint8 bucket = bucketOf(rttMs);
        Vote storage v = lastVote[msg.sender][cell][labelId];
        if (v.set && v.epoch + 1 >= epoch) _bump(cell, labelId, v.epoch, v.bucket, false);
        _bump(cell, labelId, epoch, bucket, true);
        v.epoch = epoch;
        v.bucket = bucket;
        v.set = true;
    }

    function _record(uint64 cell, Sample calldata s, uint64 ts) internal {
        emit Attested(msg.sender, cell, s.labelHash, s.rttMs, s.hitTtfbMs, s.token, s.prefix, ts);
    }

    // ---- views --------------------------------------------------------------------------------

    function bucketOf(uint32 rttMs) public pure returns (uint8) {
        uint32 b = rttMs / BUCKET_MS;
        return b >= BUCKETS ? uint8(BUCKETS - 1) : uint8(b);
    }

    function membersOf(uint64 cell) external view returns (uint256[] memory) {
        return _members[cell];
    }

    function histogram(uint64 cell, uint256 labelId, uint32 epoch) public view returns (uint16[32] memory h) {
        uint256[2] storage words = _hist[cell][labelId][epoch];
        for (uint256 b; b < BUCKETS; ++b) {
            h[b] = uint16(words[b / 16] >> ((b % 16) * 16));
        }
    }

    /// @notice The lower-middle median over today's and yesterday's votes, as the bucket midpoint, and the votes.
    function median(uint64 cell, uint256 labelId) public view returns (uint32 ms, uint32 votes) {
        uint32 epoch = uint32(block.timestamp / EPOCH);
        uint16[32] memory today = histogram(cell, labelId, epoch);
        uint16[32] memory yesterday = epoch == 0 ? today : histogram(cell, labelId, epoch - 1);
        uint256 total;
        uint256[32] memory counts;
        for (uint256 b; b < BUCKETS; ++b) {
            counts[b] = uint256(today[b]) + (epoch == 0 ? 0 : uint256(yesterday[b]));
            total += counts[b];
        }
        if (total == 0) return (0, 0);
        uint256 half = (total - 1) / 2;
        uint256 acc;
        for (uint256 b; b < BUCKETS; ++b) {
            acc += counts[b];
            if (acc > half) return (uint32(b) * BUCKET_MS + BUCKET_MS / 2, uint32(total));
        }
        return (0, uint32(total));
    }

    /// @notice A cell's members with their medians and vote counts, what a resolver steers by.
    function snapshot(uint64 cell)
        external
        view
        returns (uint256[] memory labelIds, uint32[] memory medians, uint32[] memory votes)
    {
        labelIds = _members[cell];
        medians = new uint32[](labelIds.length);
        votes = new uint32[](labelIds.length);
        for (uint256 i; i < labelIds.length; ++i) {
            (medians[i], votes[i]) = median(cell, labelIds[i]);
        }
    }

    /// @notice namehash of `<label>.<cell>.dedan.eth`.
    function aliasNode(uint64 cell, uint256 labelId) public view returns (bytes32) {
        return NameCoder.namehash(REGISTRY.cellNode(cell), bytes32(labelId));
    }

    // ---- ENS records --------------------------------------------------------------------------

    /// @notice Text records of the cell names and of the `<label>.<cell>` alias names.
    ///         Cell: `dedan.nodes` (member labels, comma-separated), `dedan.latency.<label>`, `dedan.votes.<label>`
    ///         (members only), `dedan.cell`. Alias: `dedan.latency`, `dedan.votes`, `dedan.cell`, `dedan.node`.
    ///         Empty otherwise.
    function text(bytes32 node, string calldata key) external view returns (string memory) {
        uint64 cell = cellOfNode[node];
        if (cell != 0) {
            if (_eq(key, "dedan.nodes")) return _labels(cell);
            if (_eq(key, "dedan.cell")) return LibCell.label(cell);
            if (_startsWith(key, "dedan.latency.")) return _median(cell, LibLabel.id(key[14:]), false);
            if (_startsWith(key, "dedan.votes.")) return _median(cell, LibLabel.id(key[12:]), true);
            return "";
        }
        Alias memory a = aliasOf[node];
        if (a.labelId == 0) return "";
        if (_eq(key, "dedan.latency")) return _median(a.cell, a.labelId, false);
        if (_eq(key, "dedan.votes")) return _median(a.cell, a.labelId, true);
        if (_eq(key, "dedan.cell")) return LibCell.label(a.cell);
        if (_eq(key, "dedan.node")) return REGISTRY.LABEL_STORE().getLabel(a.labelId);
        return "";
    }

    /// @notice ENSIP-6 on a cell name: the `resource` records (A, AAAA) of the members with at least `MIN_VOTES`
    ///         votes, lowest median first (ties by label id), copied from each member's name and owned by the cell's.
    /// @dev ponytail: insertion sort and one resolver call per member, fine for the tens of members a cell has.
    ///      Members are read on the Dedan resolver only: a node that pointed its name at its own resolver is not
    ///      listed here, and cannot make the cell's answer revert.
    function dnsRecord(bytes32 node, bytes32 name, uint16 resource) external view returns (bytes memory out) {
        uint64 cell = cellOfNode[node];
        if (cell == 0) return "";
        bytes memory owner = abi.encodePacked(uint8(15), LibCell.label(cell), _parentDns);
        if (name != keccak256(owner)) return "";
        (uint256[] memory ids, uint256 n) = _ranked(cell);
        for (uint256 i; i < n; ++i) {
            out = bytes.concat(out, _reowned(ids[i], resource, owner));
        }
    }

    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return interfaceId == type(ITextResolver).interfaceId || interfaceId == type(IDNSRecordResolver).interfaceId
            || interfaceId == type(IERC165).interfaceId || interfaceId == type(IERC1155Receiver).interfaceId;
    }

    /// @dev The cell names are ERC-1155 tokens of the registry minted to this contract.
    function onERC1155Received(address, address, uint256, uint256, bytes calldata) external pure returns (bytes4) {
        return IERC1155Receiver.onERC1155Received.selector;
    }

    function onERC1155BatchReceived(address, address, uint256[] calldata, uint256[] calldata, bytes calldata)
        external
        pure
        returns (bytes4)
    {
        return IERC1155Receiver.onERC1155BatchReceived.selector;
    }

    // ---- internals ----------------------------------------------------------------------------

    /// @dev The first `n` of `ids`: the members with at least `MIN_VOTES` votes, lowest median first, ties by label id.
    function _ranked(uint64 cell) internal view returns (uint256[] memory ids, uint256 n) {
        uint256[] memory members = _members[cell];
        ids = new uint256[](members.length);
        uint32[] memory ms = new uint32[](members.length);
        for (uint256 i; i < members.length; ++i) {
            (uint32 m, uint32 votes) = median(cell, members[i]);
            if (votes == 0 || votes < MIN_VOTES) continue;
            uint256 j = n++;
            for (; j > 0 && (ms[j - 1] > m || (ms[j - 1] == m && ids[j - 1] > members[i])); --j) {
                (ms[j], ids[j]) = (ms[j - 1], ids[j - 1]);
            }
            (ms[j], ids[j]) = (m, members[i]);
        }
    }

    /// @dev A member's `resource` records on the Dedan resolver: type, class, ttl and rdata kept, owner replaced.
    function _reowned(uint256 labelId, uint16 resource, bytes memory owner) internal view returns (bytes memory out) {
        string memory label = REGISTRY.LABEL_STORE().getLabel(labelId);
        bytes memory own = abi.encodePacked(uint8(bytes(label).length), label, _parentDns);
        bytes memory rrs = IDNSRecordResolver(address(REGISTRY.dedanResolver())).dnsRecord(
            NameCoder.namehash(own, 0), keccak256(own), resource
        );
        for (RRUtils.RRIterator memory it = RRUtils.iterateRRs(rrs, 0); !RRUtils.done(it); RRUtils.next(it)) {
            uint256 at = it.rdataOffset - 10;
            out = bytes.concat(out, owner, BytesUtils.substring(rrs, at, it.nextOffset - at));
        }
    }

    function _drop(uint64 cell, uint256 labelId) internal {
        uint32 idx = _memberIndex[cell][labelId];
        uint256[] storage list = _members[cell];
        uint256 last = list[list.length - 1];
        list[idx - 1] = last;
        _memberIndex[cell][last] = idx;
        list.pop();
        delete _memberIndex[cell][labelId];
        delete aliasOf[aliasNode(cell, labelId)];
        emit Left(cell, bytes32(labelId));
    }

    function _bump(uint64 cell, uint256 labelId, uint32 epoch, uint8 bucket, bool up) internal {
        uint256[2] storage words = _hist[cell][labelId][epoch];
        uint256 shift = (bucket % 16) * 16;
        uint256 w = words[bucket / 16];
        uint256 c = (w >> shift) & 0xffff;
        if (up) {
            if (c == 0xffff) return; // saturate: sixty-five thousand humans in one 8 ms bucket in one cell in a day
            c += 1;
        } else {
            if (c == 0) return;
            c -= 1;
        }
        words[bucket / 16] = (w & ~(uint256(0xffff) << shift)) | (c << shift);
    }

    /// @dev Members only, like `snapshot`: a node that left keeps its votes for two days but no record.
    function _median(uint64 cell, uint256 labelId, bool votesNotMs) internal view returns (string memory) {
        if (_memberIndex[cell][labelId] == 0) return "";
        (uint32 ms, uint32 votes) = median(cell, labelId);
        if (votes == 0) return "";
        return Strings.toString(uint256(votesNotMs ? votes : ms));
    }

    function _labels(uint64 cell) internal view returns (string memory out) {
        uint256[] storage ids = _members[cell];
        for (uint256 i; i < ids.length; ++i) {
            string memory l = REGISTRY.LABEL_STORE().getLabel(ids[i]);
            out = i == 0 ? l : string.concat(out, ",", l);
        }
    }

    function _eq(string calldata a, string memory b) internal pure returns (bool) {
        return keccak256(bytes(a)) == keccak256(bytes(b));
    }

    function _startsWith(string calldata a, string memory prefix) internal pure returns (bool) {
        bytes memory p = bytes(prefix);
        if (bytes(a).length <= p.length) return false;
        return keccak256(bytes(a[:p.length])) == keccak256(p);
    }
}
