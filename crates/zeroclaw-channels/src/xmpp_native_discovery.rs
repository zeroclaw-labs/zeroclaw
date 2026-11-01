//! Reader-owned, bounded IQ exchanges; no detached reader or long-lived policy cache.
use super::*;
use std::collections::VecDeque;
use tokio_xmpp::minidom::Element;
const PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const LEGACY: &str = "eu.siacs.conversations.axolotl";
fn rejected_reply(expected: &jid::BareJid, own: &jid::BareJid) -> anyhow::Error {
    if expected != own {
        return crate::xmpp_omemo::RejectedInbound.into();
    }
    ::zeroclaw_log::record!(
        ERROR,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
        "Invalid own-account discovery or publication reply"
    );
    anyhow::Error::msg("Invalid own-account discovery or publication reply")
}
fn response_sender(from: Option<&jid::Jid>, expected: &jid::BareJid, own: &jid::BareJid) -> bool {
    match from {
        Some(from) => {
            from.to_bare() == *expected
                || expected == own && from.to_string() == own.domain().as_str()
        }
        None => expected == own,
    }
}
enum Reply {
    Result(Option<Element>),
    Absent,
}
async fn exchange<R>(
    source: &mut R,
    requests: &mpsc::Sender<Outbound>,
    pending: &mut VecDeque<Stanza>,
    request: Iq,
    expected: &jid::BareJid,
    own: &jid::BareJid,
    id: &str,
    missing: bool,
) -> anyhow::Result<Reply>
where
    R: Stream<Item = Result<Stanza, ReadError>> + Unpin,
{
    requests
        .try_send(Outbound::Control(Box::new(Stanza::Iq(request))))
        .context("XMPP discovery write queue full")?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let stanza = match source
                .next()
                .await
                .context("XMPP disconnected during discovery")?
            {
                Ok(stanza) => stanza,
                Err(ReadError::SoftTimeout) => continue,
                Err(error) => bail!("XMPP discovery read failed: {error}"),
            };
            match stanza {
                Stanza::Iq(Iq::Result {
                    id: reply,
                    from,
                    to,
                    payload,
                }) if reply == id => {
                    ensure!(
                        response_sender(from.as_ref(), expected, own),
                        rejected_reply(expected, own)
                    );
                    ensure!(
                        to.as_ref().is_none_or(|j| j.to_bare() == *own),
                        rejected_reply(expected, own)
                    );
                    return Ok(Reply::Result(payload));
                }
                Stanza::Iq(iq @ Iq::Error { .. }) => {
                    if let Iq::Error {
                        id: ref reply,
                        ref from,
                        ref to,
                        ..
                    } = iq
                    {
                        if reply != id {
                            ensure!(pending.len() < 32, "XMPP discovery receive queue full");
                            pending.push_back(Stanza::Iq(iq));
                            continue;
                        }
                        ensure!(
                            response_sender(from.as_ref(), expected, own),
                            rejected_reply(expected, own)
                        );
                        ensure!(
                            to.as_ref().is_none_or(|j| j.to_bare() == *own),
                            rejected_reply(expected, own)
                        );
                    }
                    let element = Element::from(iq);
                    let absent = element
                        .get_child("error", ns::JABBER_CLIENT)
                        .is_some_and(|e| {
                            e.get_child("item-not-found", "urn:ietf:params:xml:ns:xmpp-stanzas")
                                .is_some()
                        });
                    ensure!(missing && absent, rejected_reply(expected, own));
                    return Ok(Reply::Absent);
                }
                stanza => {
                    ensure!(pending.len() < 32, "XMPP discovery receive queue full");
                    pending.push_back(stanza);
                }
            }
        }
    })
    .await
    .map_err(|error| {
        if expected != own {
            crate::xmpp_omemo::RejectedInbound.into()
        } else {
            anyhow::Error::new(error).context("XMPP discovery deadline exceeded")
        }
    })?
}
async fn fetch<R>(
    source: &mut R,
    requests: &mpsc::Sender<Outbound>,
    pending: &mut VecDeque<Stanza>,
    own: &jid::BareJid,
    peer: &jid::BareJid,
    node: &str,
    name: &str,
    missing: bool,
) -> anyhow::Result<Option<String>>
where
    R: Stream<Item = Result<Stanza, ReadError>> + Unpin,
{
    let id = uuid::Uuid::new_v4().to_string();
    let payload = Element::builder("pubsub", PUBSUB)
        .append(
            Element::builder("items", PUBSUB)
                .attr("node".try_into()?, node)
                .build(),
        )
        .build();
    let iq = Iq::Get {
        from: None,
        to: Some(peer.clone().into()),
        id: id.clone(),
        payload,
    };
    let reply = exchange(source, requests, pending, iq, peer, own, &id, missing).await?;
    let parsed = (|| -> anyhow::Result<Option<String>> {
        let payload = match reply {
            Reply::Absent => return Ok(None),
            Reply::Result(Some(payload)) => payload,
            Reply::Result(None) => bail!("Missing discovery payload"),
        };
        ensure!(
            payload.is("pubsub", PUBSUB) && payload.children().count() == 1,
            "Ambiguous discovery result"
        );
        let items = payload
            .get_child("items", PUBSUB)
            .context("Missing discovery items")?;
        ensure!(
            items.attr("node") == Some(node) && items.children().count() == 1,
            "Wrong or ambiguous discovery node"
        );
        let item = items
            .get_child("item", PUBSUB)
            .context("Missing discovery item")?;
        ensure!(
            item.attr("id") == Some("current") && item.children().count() == 1,
            "Unexpected discovery item"
        );
        let value = item
            .get_child(name, LEGACY)
            .context("Missing discovery content")?;
        let mut xml = Vec::new();
        value.write_to(&mut xml)?;
        Ok(Some(String::from_utf8(xml)?))
    })();
    if peer != own {
        parsed.map_err(crate::xmpp_omemo::reject_input)
    } else {
        parsed
    }
}
async fn publish<R>(
    source: &mut R,
    requests: &mpsc::Sender<Outbound>,
    pending: &mut VecDeque<Stanza>,
    own: &jid::BareJid,
    node: &str,
    xml: &str,
) -> anyhow::Result<()>
where
    R: Stream<Item = Result<Stanza, ReadError>> + Unpin,
{
    let id = uuid::Uuid::new_v4().to_string();
    let content: Element = xml.parse()?;
    let payload = Element::builder("pubsub", PUBSUB)
        .append(
            Element::builder("publish", PUBSUB)
                .attr("node".try_into()?, node)
                .append(
                    Element::builder("item", PUBSUB)
                        .attr("id".try_into()?, "current")
                        .append(content)
                        .build(),
                )
                .build(),
        )
        .build();
    exchange(
        source,
        requests,
        pending,
        Iq::Set {
            from: None,
            to: None,
            id: id.clone(),
            payload,
        },
        own,
        own,
        &id,
        false,
    )
    .await?;
    Ok(())
}
pub(super) async fn announce<R>(
    source: &mut R,
    requests: &mpsc::Sender<Outbound>,
    pending: &mut VecDeque<Stanza>,
    own: &jid::BareJid,
    state: &Arc<Mutex<crate::xmpp_omemo::channel_state::ChannelState>>,
) -> anyhow::Result<()>
where
    R: Stream<Item = Result<Stanza, ReadError>> + Unpin,
{
    let bundle = state
        .lock()
        .map_err(|_| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                "Encrypted state lock poisoned"
            );
            anyhow::Error::msg("Encrypted state lock poisoned")
        })?
        .public_bundle()?;
    publish(
        source,
        requests,
        pending,
        own,
        &format!("{LEGACY}.bundles:{}", bundle.device),
        &bundle.xml,
    )
    .await?;
    let existing = fetch(
        source,
        requests,
        pending,
        own,
        own,
        &format!("{LEGACY}.devicelist"),
        "list",
        true,
    )
    .await?;
    let mut list = crate::xmpp_omemo::devices::DeviceList::parse(
        existing
            .as_deref()
            .unwrap_or("<list xmlns='eu.siacs.conversations.axolotl'/>"),
    )?;
    if list.include_own(bundle.device)? {
        publish(
            source,
            requests,
            pending,
            own,
            &format!("{LEGACY}.devicelist"),
            &list.xml(),
        )
        .await?;
    }
    Ok(())
}
pub(super) async fn peer_bundle<R>(
    source: &mut R,
    requests: &mpsc::Sender<Outbound>,
    pending: &mut VecDeque<Stanza>,
    own: &jid::BareJid,
    peer: &jid::BareJid,
    device: u32,
) -> anyhow::Result<String>
where
    R: Stream<Item = Result<Stanza, ReadError>> + Unpin,
{
    let list = fetch(
        source,
        requests,
        pending,
        own,
        peer,
        &format!("{LEGACY}.devicelist"),
        "list",
        false,
    )
    .await?
    .ok_or(crate::xmpp_omemo::RejectedInbound)?;
    ensure!(
        crate::xmpp_omemo::devices::DeviceList::parse(&list)
            .map_err(crate::xmpp_omemo::reject_input)?
            .contains(device),
        crate::xmpp_omemo::RejectedInbound
    );
    fetch(
        source,
        requests,
        pending,
        own,
        peer,
        &format!("{LEGACY}.bundles:{device}"),
        "bundle",
        false,
    )
    .await?
    .ok_or_else(|| crate::xmpp_omemo::RejectedInbound.into())
}
pub(super) async fn refresh<R>(
    source: &mut R,
    requests: &mpsc::Sender<Outbound>,
    pending: &mut VecDeque<Stanza>,
    own: &jid::BareJid,
    state: &Arc<Mutex<crate::xmpp_omemo::channel_state::ChannelState>>,
) -> anyhow::Result<()>
where
    R: Stream<Item = Result<Stanza, ReadError>> + Unpin,
{
    let bundle = state
        .lock()
        .map_err(|_| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail),
                "Encrypted state lock poisoned"
            );
            anyhow::Error::msg("Encrypted state lock poisoned")
        })?
        .public_bundle()?;
    publish(
        source,
        requests,
        pending,
        own,
        &format!("{LEGACY}.bundles:{}", bundle.device),
        &bundle.xml,
    )
    .await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn iq_sender_binding_is_strict_for_peer_discovery() {
        let own: jid::BareJid = "bob@example.test".parse().unwrap();
        let peer: jid::BareJid = "alice@example.test".parse().unwrap();
        assert!(!response_sender(None, &peer, &own));
        assert!(!response_sender(
            Some(&"mallory@example.test".parse().unwrap()),
            &peer,
            &own
        ));
        assert!(response_sender(
            Some(&"alice@example.test/phone".parse().unwrap()),
            &peer,
            &own
        ));
        assert!(response_sender(None, &own, &own));
    }
}

#[cfg(test)]
mod exchange_tests {
    use super::*;
    #[tokio::test]
    async fn rejects_spoofed_iq_and_preserves_interleaved_messages() {
        let own: jid::BareJid = "bob@example.test".parse().unwrap();
        let peer: jid::BareJid = "alice@example.test".parse().unwrap();
        for (from, to) in [
            ("mallory@example.test", "bob@example.test"),
            ("alice@example.test", "mallory@example.test"),
        ] {
            let (requests, _rx) = mpsc::channel(2);
            let mut pending = VecDeque::new();
            let mut source = futures_util::stream::iter(vec![Ok(Stanza::Iq(Iq::Result {
                from: Some(from.parse().unwrap()),
                to: Some(to.parse().unwrap()),
                id: "fixture-iq".into(),
                payload: None,
            }))]);
            let query = Iq::Get {
                from: None,
                to: Some(peer.clone().into()),
                id: "fixture-iq".into(),
                payload: Element::builder("items", PUBSUB).build(),
            };
            assert!(
                exchange(
                    &mut source,
                    &requests,
                    &mut pending,
                    query,
                    &peer,
                    &own,
                    "fixture-iq",
                    false
                )
                .await
                .is_err()
            );
        }
        let (requests, _rx) = mpsc::channel(2);
        let mut pending = VecDeque::new();
        let incoming = Message::new(Some(own.clone().into()));
        let mut source = futures_util::stream::iter(vec![
            Ok(Stanza::Message(incoming)),
            Ok(Stanza::Iq(Iq::Result {
                from: Some(peer.clone().into()),
                to: Some(own.clone().into()),
                id: "fixture-iq".into(),
                payload: None,
            })),
        ]);
        let query = Iq::Get {
            from: None,
            to: Some(peer.clone().into()),
            id: "fixture-iq".into(),
            payload: Element::builder("items", PUBSUB).build(),
        };
        assert!(
            exchange(
                &mut source,
                &requests,
                &mut pending,
                query,
                &peer,
                &own,
                "fixture-iq",
                false
            )
            .await
            .is_ok()
        );
        assert_eq!(pending.len(), 1);
        assert!(matches!(pending.pop_front(), Some(Stanza::Message(_))));
    }
}

#[cfg(test)]
mod absence_tests {
    use super::*;
    #[tokio::test]
    async fn empty_success_is_not_a_missing_device_list() {
        let own: jid::BareJid = "bob@example.test".parse().unwrap();
        let (requests, mut received) = mpsc::channel(2);
        let (replies, receiver) = mpsc::channel(1);
        let mut source = Box::pin(futures_util::stream::unfold(
            receiver,
            |mut rx: mpsc::Receiver<Result<Stanza, ReadError>>| async move {
                rx.recv().await.map(|item| (item, rx))
            },
        ));
        let mut pending = VecDeque::new();
        let server = async {
            let id = match received.recv().await.unwrap() {
                Outbound::Control(stanza) => match *stanza {
                    Stanza::Iq(Iq::Get { id, .. }) => id,
                    _ => panic!("Unexpected control fixture request"),
                },
                _ => panic!("Unexpected fixture request"),
            };
            replies
                .send(Ok(Stanza::Iq(Iq::Result {
                    from: None,
                    to: None,
                    id,
                    payload: None,
                })))
                .await
                .unwrap();
        };
        let (_, result) = tokio::join!(
            server,
            fetch(
                &mut source,
                &requests,
                &mut pending,
                &own,
                &own,
                "eu.siacs.conversations.axolotl.devicelist",
                "list",
                true
            )
        );
        assert!(
            result.is_err(),
            "Malformed success would erase existing devices"
        );
    }
}
