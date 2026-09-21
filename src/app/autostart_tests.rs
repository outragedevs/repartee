use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[tokio::test]
async fn autostart_routes_commands_before_batched_joins_on_each_socket() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut app = super::input::submit_typing_tests::test_app();
        app.config.general.flood_protection = false;
        let ids = ["liberachat", "ircnet2", "quakenet"];
        let mut peers = Vec::new();
        let (ready_tx, mut ready_rx) = tokio::sync::mpsc::channel(3);
        for (index, id) in ids.iter().enumerate() {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let mut config: crate::config::ServerConfig = toml::from_str(&format!(
                "label='{id}'\naddress='127.0.0.1'\nport={port}\ntls=false\nchannels=[]\nnick='fixture'"
            )).unwrap();
            config.channels = (0..60).map(|n| format!("#{id}-channel-{n:02}")).collect();
            let expected = config.channels.clone();
            config.channels[0].push_str(" fixture-key");
            config.autosendcmd = Some(format!("OPER {id} fixture-password; /quote MODE $N +i"));
            app.config.servers.insert((*id).into(), config);
            let ready = ready_tx.clone();
            let id = (*id).to_string();
            peers.push(tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let (read, mut write) = socket.into_split();
                let mut lines = BufReader::new(read).lines();
                while let Some(line) = lines.next_line().await.unwrap() {
                    if line.starts_with("USER ") { break; }
                }
                if index == 1 {
                    write.write_all(b":fixture 421 fixture CAP :Unknown command\r\n").await.unwrap();
                }
                write.write_all(b":fixture 001 fixture :Welcome\r\n:fixture 376 fixture :End of MOTD\r\n").await.unwrap();
                ready.send(()).await.unwrap();
                let mut commands = Vec::new();
                let mut joined = Vec::new();
                let mut batches = 0;
                while let Some(line) = tokio::time::timeout(Duration::from_secs(3), lines.next_line()).await.unwrap_or_else(|_| panic!("{id}: commands={commands:?}, joined={joined:?}")).unwrap() {
                    if let Some(join) = line.strip_prefix("JOIN ") {
                        assert_eq!(commands, [format!("OPER {id} fixture-password"), "MODE fixture +i".into()]);
                        assert!(line.len() + 2 <= 512);
                        let mut args = join.split_whitespace();
                        let channels: Vec<_> = args.next().unwrap().split(',').map(str::to_string).collect();
                        if channels.contains(&expected[0]) {
                            assert_eq!(channels[0], expected[0]);
                            assert_eq!(args.next(), Some("fixture-key"));
                        }
                        joined.extend(channels);
                        batches += 1;
                        if joined.len() == expected.len() { break; }
                    } else if line.starts_with("OPER ") || line.starts_with("MODE ") {
                        commands.push(line);
                    }
                }
                let mut expected = expected;
                expected.sort();
                joined.sort();
                assert_eq!(joined, expected);
                assert!(batches > 1);
            }));
        }
        let ids: Vec<_> = ids.iter().map(|id| (*id).to_string()).collect();
        app.start_autoconnects(&ids);
        for _ in &ids { tokio::time::timeout(Duration::from_secs(3), ready_rx.recv()).await.expect("registration not reached").unwrap(); }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let focus = app.state.active_buffer_id.clone();
        while peers.iter().any(|peer| !peer.is_finished()) {
            tokio::select! {
                event = app.irc_rx.recv() => app.handle_irc_event(event.unwrap()),
                () = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
        }
        for peer in peers { peer.await.unwrap(); }
        assert_eq!(app.state.active_buffer_id, focus);
        for id in ids { app.cancel_connection_attempt(&id); }
    }).await.expect("autostart stalled");
}

#[test]
fn autostart_commands_stay_on_their_network_and_preserve_focus() {
    use crate::irc::handle::{IrcHandle, IrcSender};
    let mut app = crate::app::input::submit_typing_tests::test_app();
    let mut senders = Vec::new();
    for id in ["liberachat", "ircnet2", "quakenet"] {
        let cfg: crate::config::ServerConfig = toml::from_str(&format!(
            "label='{id}'\naddress='127.0.0.1'\nport=1\ntls=false\nnick='{id}-nick'\nchannels=[]"
        ))
        .unwrap();
        app.setup_connection(id, &cfg);
        let sender = IrcSender::capturing(0);
        app.irc_handles.insert(
            id.into(),
            IrcHandle::new(id.into(), sender.clone(), None, None),
        );
        senders.push(sender);
    }
    let focus = app.state.active_buffer_id.clone();
    app.execute_autosendcmd(
        "ircnet2",
        "OPER fixture fixture-password; /quote MODE $N +i",
    );
    assert!(senders[0].captured().is_empty());
    assert_eq!(
        senders[1]
            .captured()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec![
            "OPER fixture fixture-password\r\n",
            "MODE ircnet2-nick +i\r\n"
        ]
    );
    assert!(senders[2].captured().is_empty());
    assert_eq!(app.state.active_buffer_id, focus);
    app.execute_autosendcmd("missing", "OPER fixture fixture-password");
    assert!(senders[2].captured().is_empty());
}
