use std::collections::HashMap;

use zbus::{
    blocking::{Connection, MessageIterator, Proxy, fdo::DBusProxy, proxy::Builder},
    proxy::CacheProperties,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

use super::{ACCOUNT, Error, Result, SERVICE};

const DESTINATION: &str = "org.freedesktop.secrets";
const ROOT: &str = "/org/freedesktop/secrets";
const SERVICE_INTERFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_INTERFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_INTERFACE: &str = "org.freedesktop.Secret.Item";
const PROMPT_INTERFACE: &str = "org.freedesktop.Secret.Prompt";
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

pub(super) fn load() -> Result<Option<Vec<u8>>> {
    Service::connect()?.load()
}

pub(super) fn store(bytes: &[u8], should_write: impl FnOnce() -> bool) -> Result<()> {
    Service::connect()?.store(bytes, should_write)
}

pub(super) fn forget() -> Result<()> {
    Service::connect()?.forget()
}

struct Service {
    connection: Connection,
    proxy: Proxy<'static>,
}

impl Service {
    fn connect() -> Result<Self> {
        let connection = Connection::session().map_err(|_| Error::CredentialStoreUnavailable)?;
        Self::new(connection)
    }

    fn new(connection: Connection) -> Result<Self> {
        let proxy = Proxy::new_owned(connection.clone(), DESTINATION, ROOT, SERVICE_INTERFACE)
            .map_err(map_error)?;
        Ok(Self { connection, proxy })
    }

    fn object(&self, path: OwnedObjectPath, interface: &'static str) -> Result<Proxy<'static>> {
        Builder::new(&self.connection)
            .destination(DESTINATION)
            .and_then(|builder| builder.path(path))
            .and_then(|builder| builder.interface(interface))
            .and_then(|builder| builder.cache_properties(CacheProperties::No).build())
            .map_err(map_error)
    }

    fn collection(&self) -> Result<Option<Proxy<'static>>> {
        let path: OwnedObjectPath = self
            .proxy
            .call("ReadAlias", &("default",))
            .map_err(map_error)?;
        if path.as_str() == "/" {
            return Ok(None);
        }
        self.object(path, COLLECTION_INTERFACE).map(Some)
    }

    fn items(&self, collection: &Proxy<'_>) -> Result<Vec<OwnedObjectPath>> {
        collection
            .call("SearchItems", &(attributes(),))
            .map_err(map_error)
    }

    fn load(&self) -> Result<Option<Vec<u8>>> {
        let Some(collection) = self.collection()? else {
            return Ok(None);
        };
        let items = self.items(&collection)?;
        let path = match items.as_slice() {
            [] => return Ok(None),
            [path] => path.clone(),
            _ => return Err(Error::CorruptData("duplicate RuTracker credentials")),
        };
        let item = self.object(path, ITEM_INTERFACE)?;
        self.unlock(&item)?;
        let session = self.open_session()?;
        let (returned_session, parameters, bytes, _): Secret = item
            .call("GetSecret", &(session.clone(),))
            .map_err(map_error)?;
        if returned_session != session || !parameters.is_empty() {
            return Err(Error::CredentialStore);
        }
        Ok(Some(bytes))
    }

    fn store(&self, bytes: &[u8], should_write: impl FnOnce() -> bool) -> Result<()> {
        let collection = self
            .collection()?
            .ok_or(Error::CredentialStoreUnavailable)?;
        self.unlock(&collection)?;
        let session = self.open_session()?;
        let properties = HashMap::from([
            (
                "org.freedesktop.Secret.Item.Label",
                Value::from("kadr — RuTracker"),
            ),
            (
                "org.freedesktop.Secret.Item.Attributes",
                Value::from(attributes()),
            ),
        ]);
        let secret = (session, Vec::<u8>::new(), bytes, "application/octet-stream");
        if !should_write() {
            return Err(Error::CredentialStoreDenied);
        }
        let (_item, prompt): (OwnedObjectPath, OwnedObjectPath) = collection
            .call("CreateItem", &(properties, secret, true))
            .map_err(map_error)?;
        self.complete_prompt(prompt)
    }

    fn forget(&self) -> Result<()> {
        let Some(collection) = self.collection()? else {
            return Ok(());
        };
        // Also remove duplicates if another client created matching items.
        for path in self.items(&collection)? {
            let item = self.object(path, ITEM_INTERFACE)?;
            self.unlock(&item)?;
            let prompt: OwnedObjectPath = item.call("Delete", &()).map_err(map_error)?;
            self.complete_prompt(prompt)?;
        }
        Ok(())
    }

    fn open_session(&self) -> Result<OwnedObjectPath> {
        // The user-session bus is local. No external crypto dependency or
        // plaintext-on-disk fallback is needed. Disconnect closes the session.
        let (_, session): (OwnedValue, OwnedObjectPath) = self
            .proxy
            .call("OpenSession", &("plain", Value::from("")))
            .map_err(map_error)?;
        Ok(session)
    }

    fn unlock(&self, object: &Proxy<'_>) -> Result<()> {
        if !object.get_property::<bool>("Locked").map_err(map_error)? {
            return Ok(());
        }
        let (_, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = self
            .proxy
            .call("Unlock", &(vec![object.path()],))
            .map_err(map_error)?;
        self.complete_prompt(prompt)?;
        // Read directly rather than trusting a possibly stale property cache.
        if object.get_property::<bool>("Locked").map_err(map_error)? {
            return Err(Error::CredentialStoreLocked);
        }
        Ok(())
    }

    fn complete_prompt(&self, path: OwnedObjectPath) -> Result<()> {
        if path.as_str() == "/" {
            return Ok(());
        }
        // Observe both prompt completion and owner loss in the same iterator.
        // A SignalIterator alone does not terminate when its service exits.
        let mut messages = MessageIterator::from(&self.connection);
        let prompt = self.object(path.clone(), PROMPT_INTERFACE)?;
        let _completed = prompt.receive_signal("Completed").map_err(map_error)?;
        let _owners = self.proxy.receive_owner_changed().map_err(map_error)?;
        let bus = DBusProxy::new(&self.connection).map_err(map_error)?;
        let owner = bus
            .get_name_owner(DESTINATION.try_into().map_err(|_| Error::CredentialStore)?)
            .map_err(|_| Error::CredentialStoreUnavailable)?;
        let prompt = Proxy::new_owned(
            self.connection.clone(),
            owner.clone(),
            path.clone(),
            PROMPT_INTERFACE,
        )
        .map_err(map_error)?;
        // Subscriptions are active before invoking: completion can be synchronous.
        let _: () = prompt.call("Prompt", &("",)).map_err(map_error)?;
        for message in &mut messages {
            let message = message.map_err(map_error)?;
            let header = message.header();
            if header.message_type() != zbus::message::Type::Signal {
                continue;
            }
            if header.sender().map(|sender| sender.as_str()) == Some("org.freedesktop.DBus")
                && header.path().map(|value| value.as_str()) == Some("/org/freedesktop/DBus")
                && header.interface().map(|interface| interface.as_str())
                    == Some("org.freedesktop.DBus")
                && header.member().map(|member| member.as_str()) == Some("NameOwnerChanged")
            {
                let (name, old_owner, _): (String, String, String) =
                    message.body().deserialize().map_err(map_error)?;
                if name == DESTINATION && old_owner == owner.as_str() {
                    return Err(Error::CredentialStoreUnavailable);
                }
            }
            if header.sender().map(|sender| sender.as_str()) == Some(owner.as_str())
                && header.path().map(|value| value.as_str()) == Some(path.as_str())
                && header.interface().map(|interface| interface.as_str()) == Some(PROMPT_INTERFACE)
                && header.member().map(|member| member.as_str()) == Some("Completed")
            {
                let (dismissed, _): (bool, OwnedValue) =
                    message.body().deserialize().map_err(map_error)?;
                return if dismissed {
                    Err(Error::CredentialStoreDenied)
                } else {
                    Ok(())
                };
            }
        }
        Err(Error::CredentialStoreUnavailable)
    }
}

fn attributes() -> HashMap<&'static str, &'static str> {
    HashMap::from([("application", SERVICE), ("account", ACCOUNT)])
}

fn map_error(error: zbus::Error) -> Error {
    // Never retain a D-Bus reply's free-form text: services can echo input.
    if let zbus::Error::MethodError(name, _, _) = error {
        match name.as_str() {
            "org.freedesktop.Secret.Error.IsLocked" => Error::CredentialStoreLocked,
            "org.freedesktop.DBus.Error.AccessDenied" => Error::CredentialStoreDenied,
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
            | "org.freedesktop.DBus.Error.Spawn.ServiceNotFound" => {
                Error::CredentialStoreUnavailable
            }
            _ => Error::CredentialStore,
        }
    } else {
        Error::CredentialStore
    }
}

#[cfg(test)]
#[path = "linux_tests.rs"]
mod tests;
