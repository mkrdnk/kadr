//! All D-Bus traffic in these tests goes to a private daemon and fake service.
//! No connection is made to the developer's session bus or actual secret store.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};

use zbus::{blocking::connection::Builder, object_server::SignalEmitter};

use super::*;

const COLLECTION: &str = "/test/collection";
const ITEM: &str = "/test/item";
const SESSION: &str = "/test/session";
const PROMPT: &str = "/test/prompt";

#[derive(Default)]
struct State {
    bytes: Option<Vec<u8>>,
    locked: bool,
    dismiss: bool,
    no_collection: bool,
    duplicates: bool,
    fail: bool,
    disappear: bool,
}

type Shared = Arc<Mutex<State>>;

fn path(value: &str) -> OwnedObjectPath {
    value.try_into().unwrap()
}

struct FakeService(Shared);

#[zbus::interface(name = "org.freedesktop.Secret.Service")]
impl FakeService {
    fn read_alias(&self, alias: &str) -> zbus::fdo::Result<OwnedObjectPath> {
        assert_eq!(alias, "default");
        let state = self.0.lock().unwrap();
        if state.fail {
            return Err(zbus::fdo::Error::AccessDenied("private-password".into()));
        }
        Ok(path(if state.no_collection { "/" } else { COLLECTION }))
    }

    fn open_session(&self, algorithm: &str, input: OwnedValue) -> (OwnedValue, OwnedObjectPath) {
        assert_eq!(algorithm, "plain");
        assert_eq!(<&str>::try_from(&input).unwrap(), "");
        (Value::from("").try_into().unwrap(), path(SESSION))
    }

    fn unlock(&self, objects: Vec<OwnedObjectPath>) -> (Vec<OwnedObjectPath>, OwnedObjectPath) {
        assert_eq!(objects.len(), 1);
        (Vec::new(), path(PROMPT))
    }
}

struct FakeCollection(Shared);

#[zbus::interface(name = "org.freedesktop.Secret.Collection")]
impl FakeCollection {
    #[zbus(property)]
    fn locked(&self) -> bool {
        self.0.lock().unwrap().locked
    }

    fn search_items(&self, attributes: HashMap<String, String>) -> Vec<OwnedObjectPath> {
        assert_eq!(
            attributes.get("application").map(String::as_str),
            Some(SERVICE)
        );
        assert_eq!(attributes.get("account").map(String::as_str), Some(ACCOUNT));
        assert_eq!(attributes.len(), 2);
        let state = self.0.lock().unwrap();
        if state.bytes.is_none() {
            Vec::new()
        } else if state.duplicates {
            vec![path(ITEM), path(ITEM)]
        } else {
            vec![path(ITEM)]
        }
    }

    fn create_item(
        &self,
        properties: HashMap<String, OwnedValue>,
        secret: Secret,
        replace: bool,
    ) -> (OwnedObjectPath, OwnedObjectPath) {
        assert!(replace);
        assert_eq!(properties.len(), 2);
        let attrs = properties
            .get("org.freedesktop.Secret.Item.Attributes")
            .unwrap();
        let attrs = HashMap::<String, String>::try_from(attrs.try_clone().unwrap()).unwrap();
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs["application"], SERVICE);
        assert_eq!(attrs["account"], ACCOUNT);
        assert_eq!(secret.0, path(SESSION));
        assert!(secret.1.is_empty());
        assert_eq!(secret.3, "application/octet-stream");
        let mut state = self.0.lock().unwrap();
        assert!(!state.locked);
        state.bytes = Some(secret.2);
        (path(ITEM), path("/"))
    }
}

struct FakeItem(Shared);

#[zbus::interface(name = "org.freedesktop.Secret.Item")]
impl FakeItem {
    #[zbus(property)]
    fn locked(&self) -> bool {
        self.0.lock().unwrap().locked
    }

    fn get_secret(&self, session: OwnedObjectPath) -> (Secret,) {
        assert_eq!(session, path(SESSION));
        let state = self.0.lock().unwrap();
        assert!(!state.locked);
        // The response contains a *single struct*, not four top-level args.
        ((
            session,
            Vec::new(),
            state.bytes.clone().unwrap(),
            "application/octet-stream".into(),
        ),)
    }

    fn delete(&self) -> OwnedObjectPath {
        let mut state = self.0.lock().unwrap();
        assert!(!state.locked);
        state.bytes = None;
        path("/")
    }
}

struct FakePrompt(Shared);

#[zbus::interface(name = "org.freedesktop.Secret.Prompt")]
impl FakePrompt {
    async fn prompt(
        &self,
        window_id: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        assert_eq!(window_id, "");
        let disappear = self.0.lock().unwrap().disappear;
        if disappear {
            // Keep the bus and connection alive, but lose the service name
            // without ever completing the prompt.
            connection.release_name(DESTINATION).await?;
            return Ok(());
        }
        let dismiss = {
            let mut state = self.0.lock().unwrap();
            if !state.dismiss {
                state.locked = false;
            }
            state.dismiss
        };
        // Emit before returning the method reply to exercise the subscription race.
        Self::completed(&emitter, dismiss, Value::from(""))
            .await
            .map_err(Into::into)
    }

    #[zbus(signal)]
    async fn completed(
        emitter: &SignalEmitter<'_>,
        dismissed: bool,
        result: Value<'_>,
    ) -> zbus::Result<()>;
}

struct TestBus(Child);

impl Drop for TestBus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "requires dbus-daemon; isolated fake service only, never the real keychain"]
fn isolated_secret_service_lifecycle_and_failures() {
    // Do not inherit --session's service directories: even accidental calls
    // after the fake loses its name must never activate a real secret service.
    let mut config = tempfile::NamedTempFile::new().unwrap();
    config
        .write_all(
            br#"
        <busconfig>
          <type>session</type>
          <listen>unix:tmpdir=/tmp</listen>
          <auth>EXTERNAL</auth>
          <policy context="default">
            <allow user="*"/>
            <allow own="*"/>
            <allow send_destination="*"/>
            <allow receive_sender="*"/>
          </policy>
        </busconfig>
    "#,
        )
        .unwrap();
    let child = Command::new("dbus-daemon")
        .arg("--config-file")
        .arg(config.path())
        .args(["--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("dbus-daemon must be installed for this test");
    let mut bus = TestBus(child);
    let mut address = String::new();
    BufReader::new(bus.0.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let address = address.trim();
    let shared = Shared::default();
    let _server = Builder::address(address)
        .unwrap()
        .name(DESTINATION)
        .unwrap()
        .serve_at(ROOT, FakeService(shared.clone()))
        .unwrap()
        .serve_at(COLLECTION, FakeCollection(shared.clone()))
        .unwrap()
        .serve_at(ITEM, FakeItem(shared.clone()))
        .unwrap()
        .serve_at(PROMPT, FakePrompt(shared.clone()))
        .unwrap()
        .build()
        .unwrap();
    let client = Builder::address(address).unwrap().build().unwrap();
    let service = Service::new(client).unwrap();

    assert_eq!(service.load().unwrap(), None);
    service.forget().unwrap();
    let first = super::super::encode(&super::super::Credentials {
        username: "first-user".into(),
        password: "first-secret".into(),
    });
    service.store(&first, || true).unwrap();
    assert_eq!(service.load().unwrap(), Some(first));
    let second = super::super::encode(&super::super::Credentials {
        username: "second-user".into(),
        password: "second-secret".into(),
    });
    service.store(&second, || true).unwrap();
    assert_eq!(
        service.store(b"cancelled-before-write", || false),
        Err(Error::CredentialStoreDenied)
    );
    shared.lock().unwrap().locked = true;
    assert_eq!(service.load().unwrap(), Some(second.clone()));
    shared.lock().unwrap().locked = true;
    shared.lock().unwrap().dismiss = true;
    assert_eq!(service.load(), Err(Error::CredentialStoreDenied));
    assert_eq!(
        service.store(b"must-not-replace", || true),
        Err(Error::CredentialStoreDenied)
    );
    assert_eq!(service.forget(), Err(Error::CredentialStoreDenied));
    assert_eq!(shared.lock().unwrap().bytes, Some(second));
    shared.lock().unwrap().dismiss = false;
    shared.lock().unwrap().duplicates = true;
    assert_eq!(
        service.load(),
        Err(Error::CorruptData("duplicate RuTracker credentials"))
    );
    service.forget().unwrap();
    service.forget().unwrap();
    assert_eq!(service.load().unwrap(), None);
    shared.lock().unwrap().no_collection = true;
    assert_eq!(service.load().unwrap(), None);
    service.forget().unwrap();
    assert_eq!(
        service.store(b"secret", || true),
        Err(Error::CredentialStoreUnavailable)
    );
    shared.lock().unwrap().fail = true;
    let error = service.load().unwrap_err();
    assert_eq!(error, Error::CredentialStoreDenied);
    assert!(!format!("{error:?} {error}").contains("private-password"));
    assert!(std::error::Error::source(&error).is_none());
    {
        let mut state = shared.lock().unwrap();
        state.fail = false;
        state.no_collection = false;
        state.duplicates = false;
        state.bytes = Some(b"secret".to_vec());
        state.locked = true;
        state.disappear = true;
    }
    assert_eq!(service.load(), Err(Error::CredentialStoreUnavailable));
    // The bus stays alive and cannot auto-activate a real credential store.
    assert_eq!(service.load(), Err(Error::CredentialStoreUnavailable));
}
