// Copyright 2021 Red Hat, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::net::IpAddr;
use std::str::FromStr;

use futures::stream::TryStreamExt;
use netlink_packet_route::rtnl::AddressMessage;
use serde::{Deserialize, Serialize};

use crate::{
    netlink::{get_ip_addr, get_ip_prefix_len},
    Iface, IfaceConf, NisporError,
};

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
pub struct Ipv4Info {
    pub addresses: Vec<Ipv4AddrInfo>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
pub struct Ipv4AddrInfo {
    pub address: String,
    pub prefix_len: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    // The renaming seonds for this address be valid
    pub valid_lft: String,
    // The renaming seonds for this address be preferred
    pub preferred_lft: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
pub struct Ipv6Info {
    pub addresses: Vec<Ipv6AddrInfo>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
pub struct Ipv6AddrInfo {
    pub address: String,
    pub prefix_len: u8,
    // The renaming seonds for this address be valid
    pub valid_lft: String,
    // The renaming seonds for this address be preferred
    pub preferred_lft: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
pub struct IpConf {
    pub addresses: Vec<IpAddrConf>,
}

impl From<&Ipv4Info> for IpConf {
    fn from(info: &Ipv4Info) -> Self {
        let mut addrs = Vec::new();
        for addr_info in &info.addresses {
            if addr_info.valid_lft == "forever" {
                addrs.push(IpAddrConf {
                    address: addr_info.address.clone(),
                    prefix_len: addr_info.prefix_len,
                });
            }
        }
        Self { addresses: addrs }
    }
}

impl From<&Ipv6Info> for IpConf {
    fn from(info: &Ipv6Info) -> Self {
        let mut addrs = Vec::new();
        for addr_info in &info.addresses {
            if addr_info.valid_lft == "forever" {
                addrs.push(IpAddrConf {
                    address: addr_info.address.clone(),
                    prefix_len: addr_info.prefix_len,
                });
            }
        }
        Self { addresses: addrs }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum IpFamily {
    Ipv4,
    Ipv6,
}

#[derive(
    Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone, Default,
)]
pub struct IpAddrConf {
    pub address: String,
    pub prefix_len: u8,
}

impl IpConf {
    pub async fn apply(
        &self,
        handle: &rtnetlink::Handle,
        cur_iface: &Iface,
        family: IpFamily,
    ) -> Result<(), NisporError> {
        log::warn!("WARN: Deprecated, please use NetConf::apply() instead");
        let iface = match family {
            IpFamily::Ipv4 => IfaceConf {
                ipv4: Some(self.clone()),
                ..Default::default()
            },
            IpFamily::Ipv6 => IfaceConf {
                ipv6: Some(self.clone()),
                ..Default::default()
            },
        };
        let ifaces = vec![&iface];
        let mut cur_ifaces = HashMap::new();
        cur_ifaces.insert(cur_iface.name.clone(), cur_iface.clone());
        change_ips(handle, &ifaces, &cur_ifaces).await
    }
}

fn is_ipv6_unicast_link_local_full(ip: &str, prefix_len: u8) -> bool {
    is_ipv6_addr(ip)
        && ip.len() >= 3
        && ["fe8", "fe9", "fea", "feb"].contains(&&ip[..3])
        && prefix_len >= 10
}

// TODO: Rust offical has std::net::Ipv6Addr::is_unicast_link_local() in
// experimental.
fn is_ipv6_unicast_link_local(address_full: &str) -> bool {
    // The unicast link local address range is fe80::/10.
    let v: Vec<&str> = address_full.split('/').collect();
    if v.len() == 2 {
        let ip = v[0];
        if let Ok(prefix) = str::parse::<u8>(v[1]) {
            is_ipv6_unicast_link_local_full(ip, prefix)
        } else {
            false
        }
    } else {
        false
    }
}

fn is_ipv6_addr(addr: &str) -> bool {
    addr.contains(':')
}

async fn get_nl_addr_msgs(
    handle: &rtnetlink::Handle,
) -> Result<HashMap<u32, HashMap<String, AddressMessage>>, NisporError> {
    let mut msgs: HashMap<u32, HashMap<String, AddressMessage>> =
        HashMap::new();
    let mut addrs = handle.address().get().execute();
    while let Some(nl_addr_msg) = addrs.try_next().await? {
        let iface_index = nl_addr_msg.header.index;
        let full_address = format!(
            "{}/{}",
            get_ip_addr(&nl_addr_msg),
            get_ip_prefix_len(&nl_addr_msg)
        );
        match msgs.entry(iface_index) {
            Entry::Occupied(o) => {
                o.into_mut().insert(full_address, nl_addr_msg);
            }
            Entry::Vacant(v) => {
                v.insert({
                    let mut tmp = HashMap::new();
                    tmp.insert(full_address, nl_addr_msg);
                    tmp
                });
            }
        };
    }

    Ok(msgs)
}

// For ipv6 link local address,
// 1. We remove existing link ipv6 link local address when desire has ipv6 link
//    local address
// 2. We remove existing link ipv6 link local address when desire explicityly
//    said `ipv6.address = []`.
pub(crate) async fn change_ips(
    handle: &rtnetlink::Handle,
    ifaces: &[&IfaceConf],
    cur_ifaces: &HashMap<String, Iface>,
) -> Result<(), NisporError> {
    let iface_2_nl_addr_msgs = get_nl_addr_msgs(handle).await?;

    for iface in ifaces {
        if let Some(cur_iface) = cur_ifaces.get(&iface.name) {
            let nl_addr_msgs = iface_2_nl_addr_msgs.get(&cur_iface.index);
            apply_ip_conf(
                handle,
                nl_addr_msgs,
                cur_iface.index,
                iface.ipv4.as_ref(),
                cur_iface.ipv4.as_ref().map(|ip_info| ip_info.into()),
                IpFamily::Ipv4,
            )
            .await?;
            apply_ip_conf(
                handle,
                nl_addr_msgs,
                cur_iface.index,
                iface.ipv6.as_ref(),
                cur_iface.ipv6.as_ref().map(|ip_info| ip_info.into()),
                IpFamily::Ipv6,
            )
            .await?;
        }
    }

    Ok(())
}

async fn apply_ip_conf(
    handle: &rtnetlink::Handle,
    nl_addr_msgs: Option<&HashMap<String, AddressMessage>>,
    iface_index: u32,
    ip_conf: Option<&IpConf>,
    cur_ip_conf: Option<IpConf>,
    ip_family: IpFamily,
) -> Result<(), NisporError> {
    // TODO: Can we use single queue?
    match (ip_conf, cur_ip_conf) {
        (None, None) => (),
        (None, Some(_)) => {
            // Desire would like to remove all address except IPv6 link local
            // address
            if let Some(nl_addr_msgs) = nl_addr_msgs {
                for (address_full, nl_addr_msg) in nl_addr_msgs.iter() {
                    match ip_family {
                        IpFamily::Ipv4 => {
                            if !is_ipv6_addr(address_full) {
                                handle
                                    .address()
                                    .del(nl_addr_msg.clone())
                                    .execute()
                                    .await?;
                            }
                        }
                        IpFamily::Ipv6 => {
                            if is_ipv6_addr(address_full)
                                && !is_ipv6_unicast_link_local(address_full)
                            {
                                handle
                                    .address()
                                    .del(nl_addr_msg.clone())
                                    .execute()
                                    .await?;
                            }
                        }
                    };
                }
            }
        }
        (Some(ip_conf), None) => {
            // Desire would like to add more address
            for addr_conf in &ip_conf.addresses {
                handle
                    .address()
                    .add(
                        iface_index,
                        ip_addr_str_to_enum(&addr_conf.address)?,
                        addr_conf.prefix_len,
                    )
                    .execute()
                    .await?;
            }
        }
        (Some(ip_conf), Some(cur_ip_conf)) => {
            let mut cur_ip_addr_confs = HashSet::new();
            let mut des_ip_addr_confs = HashSet::new();
            for des_addr in &ip_conf.addresses {
                des_ip_addr_confs.insert(IpAddrConf {
                    address: des_addr.address.clone(),
                    prefix_len: des_addr.prefix_len,
                });
            }
            for cur_addr in &cur_ip_conf.addresses {
                cur_ip_addr_confs.insert(IpAddrConf {
                    address: cur_addr.address.clone(),
                    prefix_len: cur_addr.prefix_len,
                });
            }
            let has_ipv6_link_local_in_desire = if ip_family == IpFamily::Ipv4 {
                ip_conf.addresses.iter().any(|addr| {
                    is_ipv6_unicast_link_local_full(
                        &addr.address,
                        addr.prefix_len,
                    )
                })
            } else {
                false
            };
            for addr_to_remove in &cur_ip_addr_confs - &des_ip_addr_confs {
                // Only remove ipv6 link local address when desire has link
                // local address defined
                if !(ip_family == IpFamily::Ipv6
                    && !has_ipv6_link_local_in_desire
                    && is_ipv6_unicast_link_local_full(
                        &addr_to_remove.address,
                        addr_to_remove.prefix_len,
                    ))
                {
                    if let Some(nl_addr_msgs) = nl_addr_msgs {
                        if let Some(nl_addr_msg) = nl_addr_msgs.get(&format!(
                            "{}/{}",
                            &addr_to_remove.address, addr_to_remove.prefix_len
                        )) {
                            handle
                                .address()
                                .del(nl_addr_msg.clone())
                                .execute()
                                .await?;
                        }
                    }
                }
            }

            for addr_to_add in &des_ip_addr_confs - &cur_ip_addr_confs {
                handle
                    .address()
                    .add(
                        iface_index,
                        ip_addr_str_to_enum(&addr_to_add.address)?,
                        addr_to_add.prefix_len,
                    )
                    .execute()
                    .await?;
            }
        }
    }
    Ok(())
}

fn ip_addr_str_to_enum(address: &str) -> Result<IpAddr, NisporError> {
    Ok(if is_ipv6_addr(address) {
        IpAddr::V6(std::net::Ipv6Addr::from_str(address)?)
    } else {
        IpAddr::V4(std::net::Ipv4Addr::from_str(address)?)
    })
}
