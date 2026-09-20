use std::time::Duration;

use crate::config::{GeneralConfig, ServerConfig};
use crate::irc::{IrcEvent, connect_server};

fn general() -> GeneralConfig {
    GeneralConfig {
        flood_protection: false,
        ..GeneralConfig::default()
    }
}

async fn accepted(config: &ServerConfig, expected_network: Option<&str>) {
    let (handle, mut events) = tokio::time::timeout(
        Duration::from_secs(10),
        connect_server("fixture", config, &general()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!handle.sasl_authenticated);
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut connected = false;
        let mut network = None;
        while let Some(event) = events.recv().await {
            match event {
                IrcEvent::Connected(_, caps, _) => {
                    assert!(caps.contains(super::NETWORKS_CAP));
                    assert!(!caps.contains("sasl"));
                    connected = true;
                }
                IrcEvent::Message(_, message) => {
                    if let irc::proto::Command::Response(irc::proto::Response::RPL_ISUPPORT, args) =
                        &message.command
                    {
                        for arg in args {
                            if let Some(value) = arg.strip_prefix("BOUNCER_NETID=") {
                                network = Some(value.to_string());
                            }
                        }
                    }
                    if matches!(
                        message.command,
                        irc::proto::Command::Response(
                            irc::proto::Response::RPL_ENDOFMOTD | irc::proto::Response::ERR_NOMOTD,
                            _
                        )
                    ) {
                        assert!(connected);
                        assert_eq!(network.as_deref(), expected_network);
                        return;
                    }
                }
                IrcEvent::Disconnected(_, reason) => {
                    panic!("PASS connection disconnected: {reason:?}")
                }
                _ => {}
            }
        }
        panic!("PASS registration did not complete");
    })
    .await
    .unwrap();
    drop(handle);
}

async fn rejected(config: &ServerConfig, expected: Option<&str>) {
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        connect_server("fixture", config, &general()),
    )
    .await
    .expect("authentication rejection timed out");
    let error = match result {
        Ok(_) => panic!("invalid authentication or binding unexpectedly succeeded"),
        Err(error) => error.to_string(),
    };
    if let Some(expected) = expected {
        assert!(error.contains(expected), "unexpected error: {error}");
    }
    assert!(!error.contains("fixture-password"));
    assert!(!error.contains("wrong-password"));
}

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
async fn pinned_bouncer_pass_registration() {
    let lurker = std::env::var("REPARTEE_BOUNCER_TEST_PROVIDER").unwrap() == "lurker";
    let mut config: ServerConfig = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=['#must-not-autojoin']",
    )
    .unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let user = std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap();
    let network = std::env::var("REPARTEE_BOUNCER_TEST_NETID").unwrap();
    config.username = Some(user.clone());
    config.password = Some("fixture-password".into());
    config.bouncer_control = true;
    accepted(&config, None).await;
    config.bouncer_control = false;
    config.bouncer_network_id = Some(network.clone());
    if lurker {
        accepted(&config, Some(&network)).await;
        accepted(&config, Some(&network)).await;
        config.username = Some("ignored".into());
        config.password = Some(format!("{user}:fixture-password"));
        accepted(&config, Some(&network)).await;
        config.username = Some(user.clone());
        config.password = Some("fixture-password".into());
        config.bouncer_network_id = Some("9223372036854775807".into());
        rejected(&config, None).await;
        config.bouncer_network_id = Some(network.clone());
        config.password = Some("wrong-password".into());
        rejected(&config, None).await;
    } else {
        rejected(&config, Some("Account authentication is required")).await;
    }
    config.password = None;
    rejected(&config, None).await;
    config.sasl_user = Some(user);
    config.sasl_pass = Some("wrong-password".into());
    rejected(&config, None).await;
    config.password = Some("fixture-password".into());
    rejected(&config, Some("remove SASL settings")).await;
    config.bouncer_network_id = None;
    config.bouncer_control = true;
    config.password = Some("fixture-password".into());
    rejected(&config, Some("remove SASL settings")).await;
    config.sasl_user = None;
    config.sasl_pass = None;
    config.password = Some("wrong-password".into());
    rejected(&config, None).await;
}
