//! A very basic client for Hashicorp's Vault

use backend::{Backend, BoxedError, err};
use hyper;
use rustc_serialize::json;
use secretfile::{Location, Secretfile};
use std::collections::BTreeMap;
use std::env;
use std::fs::File;
use std::io::Read;

// Define our custom vault token header for use with hyper.
header! { (XVaultToken, "X-Vault-Token") => [String] }

/// The default vault server address.
fn default_addr() -> Result<String, BoxedError> {
    env::var("VAULT_ADDR").map_err(|_| {
        err("VAULT_ADDR not specified")
    })
}

/// The default vault token.
fn default_token() -> Result<String, BoxedError> {
    env::var("VAULT_TOKEN").or_else(|_| {
        // Build a path to ~/.vault-token.
        let mut path = try!(env::home_dir().ok_or_else(|| {
            return err("Can't find home directory")
        }));
        path.push(".vault-token");

        // Read the file.
        let mut f = try!(File::open(path));
        let mut token = String::new();
        try!(f.read_to_string(&mut token));
        Ok(token)
    }).map_err(|_: BoxedError| {
        err("Cannot get either VAULT_TOKEN or ~/.vault_token")
    })
}

/// Secret data retrieved from Vault.  This has a bunch more fields, but
/// the exact list of fields doesn't seem to be documented anywhere, so
/// let's be conservative.
#[derive(Debug, RustcDecodable)]
struct Secret {
    /// The key-value pairs associated with this secret.
    data: BTreeMap<String, String>,
    // How long this secret will remain valid for, in seconds.
    lease_duration: u64,
}

/// A basic Vault client.
pub struct Client {
    /// Our HTTP client.  This can be configured to mock out the network.
    client: hyper::Client,
    /// The address of our Vault server.
    addr: hyper::Url,
    /// The token which we'll use to access Vault.
    token: String,
    /// Mapping from environment-variable-style names to locations in
    /// Vault.
    secretfile: Secretfile,
    /// Local cache of secrets.
    secrets: BTreeMap<String, Secret>,
}

impl Client {
    /// Has the user indicated that they want to enable our Vault backend?
    pub fn is_enabled() -> bool {
        default_addr().is_ok()
    }

    /// Construct a new vault::Client, attempting to use the same
    /// environment variables and files used by the `vault` CLI tool and
    /// the Ruby `vault` gem.
    pub fn new_default() -> Result<Client, BoxedError> {
        Client::new(hyper::Client::new(),
                    &try!(default_addr()),
                    try!(default_token()),
                    try!(Secretfile::default()))
    }

    fn new<U,S>(client: hyper::Client, addr: U, token: S,
                secretfile: Secretfile) ->
        Result<Client, BoxedError>
        where U: hyper::client::IntoUrl, S: Into<String>
    {
        Ok(Client {
            client: client,
            addr: try!(addr.into_url()),
            token: token.into(),
            secretfile: secretfile,
            secrets: BTreeMap::new(),
        })
    }

    fn get_secret(&self, path: &str) -> Result<Secret, BoxedError> {
        let url = try!(self.addr.join(&format!("v1/{}", path)));

        let req = self.client.get(url)
            .header(XVaultToken(self.token.clone()));
        let mut res = try!(req.send());

        let mut body = String::new();
        try!(res.read_to_string(&mut body));
        Ok(try!(json::decode(&body)))
    }
}

impl Backend for Client {
    fn get(&mut self, credential: &str) -> Result<String, BoxedError> {
        match self.secretfile.get(credential) {
            None => {
                let msg = format!("No Secretfile entry for {}", credential);
                Err(err(msg))
            }
            Some(&Location::Keyed(ref path, ref key)) => {
                // If we haven't cached this secret, do so.  This is
                // necessary to correctly support dynamic credentials,
                // which may have more than one related key in a single
                // secret, and fetching the secret once per key will result
                // in mismatched username/password pairs or whatever.
                if !self.secrets.contains_key(path) {
                    let secret = try!(self.get_secret(path));
                    self.secrets.insert(path.to_owned(), secret);
                }

                // Get the secret from our cache.  `unwrap` is safe here,
                // because if we didn't have it, we grabbed it above.
                let secret = self.secrets.get(path).unwrap();

                // Look up the specified key in our secret's data bag.
                secret.data.get(key).ok_or_else(|| {
                    err(format!("No key {} in secret {}", key, path))
                }).map(|v| v.clone())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use backend::Backend;
    use hyper;
    use secretfile::Secretfile;
    use super::Client;

    mock_connector!(MockVault {
        "http://127.0.0.1" =>
          "HTTP/1.1 200 OK\r\n\
           Content-Type: application/json\r\n\
           \r\n\
           {\"data\": {\"value\": \"bar\"},\"lease_duration\": 2592000}\r\n\
           "
    });

    fn test_client() -> Client {
        let h = hyper::Client::with_connector(MockVault::default());
        let secretfile = Secretfile::from_str("FOO secret/foo:value").unwrap();
        Client::new(h, "http://127.0.0.1", "123", secretfile).unwrap()
    }

    #[test]
    fn test_get_secret() {
        let client = test_client();
        let secret = client.get_secret("secret/foo").unwrap();
        assert_eq!("bar", secret.data.get("value").unwrap());
    }

    #[test]
    fn test_get() {
        let mut client = test_client();
        assert_eq!("bar", client.get("FOO").unwrap());
    }
}