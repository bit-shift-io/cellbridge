use std::net::SocketAddr;

#[derive(Clone)]
pub enum Scenario {
    Fetch {
        destination: SocketAddr,
        host: String,
        path: String,
    },
    Handshake,
    Tcp {
        destination: SocketAddr,
        request: Vec<u8>,
        expected: Vec<u8>,
    },
    Udp {
        destination: SocketAddr,
        payload: Vec<u8>,
        expected: Vec<u8>,
    },
    Dns {
        resolver: SocketAddr,
        name: String,
    },
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioResult {
    pub name: String,
    pub passed: bool,
    pub elapsed_ms: u64,
    pub received_bytes: usize,
    pub message: String,
    // Only filled by an explicit TCP fetch, for egress-IP evidence.
    pub response_text: Option<String>,
}

pub(super) fn dns_query(name: &str, id: u16) -> Result<Vec<u8>, String> {
    let mut out = vec![0; 12];
    out[..2].copy_from_slice(&id.to_be_bytes());
    out[2] = 1;
    out[5] = 1;
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 || !label.is_ascii() {
            return Err("invalid DNS name".into());
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    if out.len() > 266 {
        return Err("DNS name too long".into());
    }
    out.extend_from_slice(&[0, 0, 1, 0, 1]);
    Ok(out)
}

type DnsName = Vec<Vec<u8>>;

fn dns_name(bytes: &[u8], mut offset: usize) -> Option<(DnsName, usize)> {
    let mut labels = vec![];
    let mut end = None;
    let mut visited = std::collections::HashSet::new();
    loop {
        if !visited.insert(offset) || visited.len() > 128 {
            return None;
        }
        let length = *bytes.get(offset)?;
        if length & 0xc0 == 0xc0 {
            let target = ((length as usize & 0x3f) << 8) | *bytes.get(offset + 1)? as usize;
            if target >= offset {
                return None;
            }
            end.get_or_insert(offset + 2);
            offset = target;
        } else {
            if length > 63 {
                return None;
            }
            offset += 1;
            if length == 0 {
                return Some((labels, end.unwrap_or(offset)));
            }
            let label = bytes.get(offset..offset + length as usize)?;
            labels.push(label.iter().map(u8::to_ascii_lowercase).collect());
            offset += length as usize;
        }
    }
}

pub(super) fn valid_dns(bytes: &[u8], query: &[u8]) -> bool {
    let header_ok = bytes.len() >= query.len()
        && bytes[..2] == query[..2]
        && bytes[2] & 0x80 != 0
        && bytes[2] & 0x78 == 0
        && bytes[3] & 0x0f == 0
        && bytes[2] & 0x02 == 0
        && bytes[4..6] == [0, 1]
        && u16::from_be_bytes([bytes[6], bytes[7]]) > 0
        && bytes[12..query.len()] == query[12..]
        && bytes.len() > query.len();
    if !header_ok {
        return false;
    }
    let mut offset = query.len();
    let mut addresses = std::collections::HashSet::new();
    let mut aliases = std::collections::HashMap::new();
    let answer_count = u16::from_be_bytes([bytes[6], bytes[7]]);
    for _ in 0..answer_count {
        let Some((owner, next)) = dns_name(bytes, offset) else {
            return false;
        };
        offset = next;
        let Some(record) = bytes.get(offset..offset + 10) else {
            return false;
        };
        let length = u16::from_be_bytes([record[8], record[9]]) as usize;
        offset += 10;
        if bytes.get(offset..offset + length).is_none() {
            return false;
        }
        if record[..4] == [0, 1, 0, 1] && length == 4 {
            addresses.insert(owner);
        } else if record[..4] == [0, 5, 0, 1] {
            let Some((target, next)) = dns_name(bytes, offset) else {
                return false;
            };
            if next != offset + length {
                return false;
            }
            aliases.insert(owner, target);
        }
        offset += length;
    }
    let Some((mut name, _)) = dns_name(query, 12) else {
        return false;
    };
    for _ in 0..=answer_count {
        if addresses.contains(&name) {
            return true;
        }
        let Some(target) = aliases.get(&name) else {
            return false;
        };
        name = target.clone();
    }
    false
}

#[cfg(test)]
mod tests {
    #[test]
    fn dns_requires_matching_question_transaction_and_complete_answer() {
        let query = super::dns_query("example.com", 0x1234).unwrap();
        let mut reply = query.clone();
        reply[2] = 0x81;
        reply[3] = 0x80;
        reply[7] = 1;
        reply.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 93, 184, 216, 34]);
        assert!(super::valid_dns(&reply, &query));
        let mut wrong = reply.clone();
        wrong[0] ^= 1;
        assert!(!super::valid_dns(&wrong, &query));
        wrong = reply.clone();
        wrong[2] &= 0x7f;
        assert!(!super::valid_dns(&wrong, &query));
        wrong = reply.clone();
        wrong[3] |= 3;
        assert!(!super::valid_dns(&wrong, &query));
        wrong = reply.clone();
        wrong[2] |= 0x02;
        assert!(
            !super::valid_dns(&wrong, &query),
            "truncated DNS must fail even with an A record"
        );
        let mut unrelated = reply[..query.len()].to_vec();
        unrelated.extend_from_slice(b"\x05other\x03com\0");
        unrelated.extend_from_slice(&reply[query.len() + 2..]);
        assert!(
            !super::valid_dns(&unrelated, &query),
            "unrelated A owner must not pass"
        );
        assert!(!super::valid_dns(&reply[..reply.len() - 1], &query));
    }

    #[test]
    fn dns_accepts_an_address_reached_through_a_valid_cname_chain() {
        let query = super::dns_query("example.com", 0x1234).unwrap();
        let target_query = super::dns_query("alias.example.com", 0).unwrap();
        let target = &target_query[12..target_query.len() - 4];
        let mut reply = query.clone();
        reply[2] = 0x81;
        reply[3] = 0x80;
        reply[7] = 2;
        reply.extend_from_slice(&[0xc0, 0x0c, 0, 5, 0, 1, 0, 0, 0, 60]);
        reply.extend_from_slice(&(target.len() as u16).to_be_bytes());
        reply.extend_from_slice(target);
        reply.extend_from_slice(target);
        reply.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 93, 184, 216, 34]);
        assert!(super::valid_dns(&reply, &query));
    }
}
