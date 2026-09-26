//! The DNS side: the authoritative zone (`<account>` steered from the map, `<label>.node` from ENS,
//! `_acme-challenge` from the management API, SOA/NS) and the customers' zone files, whose CNAMEs into our
//! zone are chased inside the answer so a stub resolver or an ACME CA gets the whole chain in one reply.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use dedan_core::geo;
use dedan_core::map::GeoLocate;
use dedan_core::token;
use dedan_core::types::*;
use hickory_proto::op::{Edns, Header, HeaderCounts, Metadata, ResponseCode};
use hickory_proto::rr::rdata::opt::{EdnsCode, EdnsOption};
use hickory_proto::rr::rdata::{A, AAAA, CNAME, NS, SOA, TXT};
use hickory_proto::rr::{Name, RData, Record, RecordType};
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use hickory_server::zone_handler::MessageResponseBuilder;
use tracing::{debug, warn};

use crate::state::State;

const TTL_STEER: u32 = 30;
const TTL_NODE: u32 = 60;
const ANSWERS: usize = 3;

pub struct Handler {
    state: Arc<State>,
    zone: String,
    /// fqdn → CNAME target, both lowercase with trailing dots, from the customers' zone files.
    cnames: HashMap<String, String>,
    /// The customer zones' origins, lowercase with trailing dots.
    customer_zones: Vec<String>,
}

struct Answer {
    rcode: ResponseCode,
    records: Vec<Record>,
    authority: Vec<Record>,
}

impl Answer {
    fn empty(rcode: ResponseCode) -> Self {
        Self {
            rcode,
            records: vec![],
            authority: vec![],
        }
    }
}

/// The two-record zone files a customer's DNS provider would serve: `$ORIGIN`, `$TTL`, `name [IN] CNAME target`.
pub fn parse_zone(origin: &str, text: &str) -> HashMap<String, String> {
    let mut origin = format!("{}.", origin.trim_end_matches('.').to_lowercase());
    let fq = |name: &str, origin: &str| -> String {
        let n = name.to_lowercase();
        if n == "@" {
            origin.to_owned()
        } else if n.ends_with('.') {
            n
        } else {
            format!("{n}.{origin}")
        }
    };
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.split(';').next().unwrap_or("").trim();
        let f: Vec<&str> = line.split_whitespace().collect();
        match f.as_slice() {
            ["$ORIGIN", o] => origin = format!("{}.", o.trim_end_matches('.').to_lowercase()),
            [name, .., "CNAME", target] | [name, .., "cname", target] => {
                out.insert(fq(name, &origin), fq(target, &origin));
            }
            _ => {}
        }
    }
    out
}

/// A `ResponseInfo` for a reply that never left: one query, nothing else.
fn info(metadata: Metadata) -> ResponseInfo {
    ResponseInfo::from(Header {
        metadata,
        counts: HeaderCounts {
            queries: 1,
            answers: 0,
            authorities: 0,
            additionals: 0,
        },
    })
}

fn name(s: &str) -> Name {
    Name::from_ascii(s).unwrap_or_else(|_| Name::root())
}

impl Handler {
    pub fn new(state: Arc<State>) -> Self {
        let mut cnames = HashMap::new();
        let mut customer_zones = Vec::new();
        if let Ok(dir) = std::fs::read_dir(&state.cfg.zones_dir) {
            for entry in dir.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "zone") {
                    let origin = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    cnames.extend(parse_zone(&origin, &text));
                    customer_zones.push(format!("{}.", origin.to_lowercase()));
                }
            }
        }
        debug!(
            cnames = cnames.len(),
            zones = customer_zones.len(),
            "customer zones loaded"
        );
        Self {
            zone: state.cfg.zone_dot(),
            state,
            cnames,
            customer_zones,
        }
    }

    async fn answer(&self, qname: &str, qtype: RecordType, client: IpAddr) -> Answer {
        if let Some(target) = self.cnames.get(qname) {
            let mut a = Answer::empty(ResponseCode::NoError);
            a.records.push(Record::from_rdata(
                name(qname),
                300,
                RData::CNAME(CNAME(name(target))),
            ));
            if qtype != RecordType::CNAME && target.ends_with(&self.zone) {
                a.records
                    .extend(self.zone_answer(target, qtype, client).await.records);
            }
            return a;
        }
        if qname == self.zone || qname.ends_with(&format!(".{}", self.zone)) {
            return self.zone_answer(qname, qtype, client).await;
        }
        if self
            .customer_zones
            .iter()
            .any(|z| qname == *z || qname.ends_with(&format!(".{z}")))
        {
            return Answer::empty(ResponseCode::NXDomain);
        }
        Answer::empty(ResponseCode::Refused)
    }

    async fn zone_answer(&self, qname: &str, qtype: RecordType, client: IpAddr) -> Answer {
        let rel = qname
            .strip_suffix(&self.zone)
            .unwrap_or("")
            .trim_end_matches('.');
        let mut a = Answer::empty(ResponseCode::NoError);
        let soa = || {
            Record::from_rdata(
                name(&self.zone),
                300,
                RData::SOA(SOA::new(
                    name(&format!("ns1.{}", self.zone)),
                    name(&format!("hostmaster.{}", self.zone)),
                    (crate::state::unix_now() / 60) as u32,
                    3600,
                    600,
                    86_400,
                    30,
                )),
            )
        };
        if rel.is_empty() {
            match qtype {
                RecordType::SOA => a.records.push(soa()),
                RecordType::NS => a.records.push(Record::from_rdata(
                    name(&self.zone),
                    3600,
                    RData::NS(NS(name(&format!("ns1.{}", self.zone)))),
                )),
                RecordType::A => a.records.push(Record::from_rdata(
                    name(qname),
                    3600,
                    RData::A(A(self.state.cfg.ns_ip)),
                )),
                _ => a.authority.push(soa()),
            }
            return a;
        }
        if rel == "ns1" {
            if qtype == RecordType::A {
                a.records.push(Record::from_rdata(
                    name(qname),
                    3600,
                    RData::A(A(self.state.cfg.ns_ip)),
                ));
            }
            return a;
        }
        if let Some(rest) = rel.strip_prefix("_acme-challenge.") {
            let _ = rest;
            if qtype == RecordType::TXT {
                for v in self
                    .state
                    .txt
                    .read()
                    .await
                    .get(qname)
                    .cloned()
                    .unwrap_or_default()
                {
                    a.records.push(Record::from_rdata(
                        name(qname),
                        0,
                        RData::TXT(TXT::new(vec![v])),
                    ));
                }
            }
            return a;
        }
        if let Some(label) = rel.strip_suffix(".node") {
            let snap = self.state.snap.read().await;
            let Some(rec) = snap.records.get(&NodeId::new(label)) else {
                return Answer::empty(ResponseCode::NXDomain);
            };
            match qtype {
                RecordType::A => {
                    if let Some(ip) = rec.ipv4 {
                        a.records
                            .push(Record::from_rdata(name(qname), TTL_NODE, RData::A(A(ip))));
                    }
                }
                RecordType::AAAA => {
                    if let Some(ip) = rec.ipv6 {
                        a.records.push(Record::from_rdata(
                            name(qname),
                            TTL_NODE,
                            RData::AAAA(AAAA(ip)),
                        ));
                    }
                }
                _ => {}
            }
            return a;
        }
        if rel.contains('.') {
            return Answer::empty(ResponseCode::NXDomain);
        }
        // <account>: the slot set, ordered by the latency map for the client's cell
        let account = AccountId::new(rel);
        let snap = self.state.snap.read().await;
        let Some(set) = snap.assignment.sets.get(&account) else {
            return if self.state.accounts.iter().any(|x| x.id == account) {
                a
            } else {
                Answer::empty(ResponseCode::NXDomain)
            };
        };
        if qtype != RecordType::A {
            a.authority.push(soa());
            return a;
        }
        let mut live: Vec<(&NodeId, std::net::Ipv4Addr)> = set
            .iter()
            .filter(|n| snap.identity.node_active(n))
            .filter_map(|n| snap.records.get(n).and_then(|r| r.ipv4).map(|ip| (n, ip)))
            .collect();
        let point = self
            .state
            .geo
            .read()
            .expect("geo lock")
            .locate(&token::prefix(client));
        let cell = point.and_then(|p| geo::cell(p).ok());
        match cell.and_then(|c| snap.map.rank(c)) {
            Some(rank) => {
                let pos = |n: &NodeId| {
                    rank.nodes
                        .iter()
                        .position(|(m, _)| m == n)
                        .unwrap_or(usize::MAX)
                };
                live.sort_by(|(x, _), (y, _)| pos(x).cmp(&pos(y)).then_with(|| x.cmp(y)));
            }
            None => {
                // cold start: nothing measured near the client, order by distance to the nodes' own locations
                if let Some(p) = point {
                    let km = |n: &NodeId| {
                        snap.records
                            .get(n)
                            .and_then(|r| r.location)
                            .map(|l| geo::distance_km(p, l))
                            .unwrap_or(f64::MAX)
                    };
                    live.sort_by(|(x, _), (y, _)| km(x).total_cmp(&km(y)).then_with(|| x.cmp(y)));
                }
            }
        }
        for (_, ip) in live.into_iter().take(ANSWERS) {
            a.records
                .push(Record::from_rdata(name(qname), TTL_STEER, RData::A(A(ip))));
        }
        a
    }
}

#[async_trait::async_trait]
impl RequestHandler for Handler {
    async fn handle_request<R: ResponseHandler, T: hickory_server::net::runtime::Time>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        let Some(query) = request.queries.queries().first() else {
            let mut m = Metadata::response_from_request(&request.metadata);
            m.response_code = ResponseCode::FormErr;
            return info(m);
        };
        let qname = query.name().to_string().to_lowercase();
        let qtype = query.query_type();
        let subnet = request
            .edns
            .as_ref()
            .and_then(|e| match e.options().get(EdnsCode::Subnet) {
                Some(EdnsOption::Subnet(s)) => Some(*s),
                _ => None,
            });
        let client = subnet
            .as_ref()
            .map(|s| s.addr())
            .unwrap_or_else(|| request.src().ip());
        let ans = self.answer(&qname, qtype, client).await;
        debug!(%qname, ?qtype, %client, rcode = ?ans.rcode, answers = ans.records.len(), "query");

        let mut metadata = Metadata::response_from_request(&request.metadata);
        metadata.authoritative = ans.rcode != ResponseCode::Refused;
        metadata.response_code = ans.rcode;
        let edns = subnet.map(|s| {
            let mut echoed = s;
            let cap = if s.addr().is_ipv4() { 24 } else { 48 };
            echoed.set_scope_prefix(s.source_prefix().min(cap));
            let mut e = Edns::new();
            e.set_max_payload(1232);
            e.options_mut().insert(EdnsOption::Subnet(echoed));
            e
        });
        let mut builder = MessageResponseBuilder::from_message_request(request);
        if let Some(e) = &edns {
            builder.edns(e);
        }
        let response = builder.build(
            metadata,
            ans.records.iter(),
            ans.authority.iter(),
            [].iter(),
            [].iter(),
        );
        match response_handle.send_response(response).await {
            Ok(info) => info,
            Err(e) => {
                warn!(error = %e, "failed to send response");
                let mut m = Metadata::response_from_request(&request.metadata);
                m.response_code = ResponseCode::ServFail;
                info(m)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_files_parse_to_cnames() {
        let text = "; comment\n$ORIGIN images.com.\n$TTL 300\nwww IN CNAME acme.dedan-cdn.test.\n_acme-challenge.www  IN CNAME _acme-challenge.acme.dedan-cdn.test.\n@ IN CNAME other.example.\n";
        let m = parse_zone("images.com", text);
        assert_eq!(m["www.images.com."], "acme.dedan-cdn.test.");
        assert_eq!(
            m["_acme-challenge.www.images.com."],
            "_acme-challenge.acme.dedan-cdn.test."
        );
        assert_eq!(m["images.com."], "other.example.");
        assert_eq!(m.len(), 3);
    }
}
