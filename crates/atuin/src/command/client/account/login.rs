use std::{io, path::PathBuf};

use clap::Parser;
use eyre::{Context, Result, bail};
use tokio::{fs::File, io::AsyncWriteExt};

use atuin_client::{
    api_client,
    encryption::{Key, decode_key, encode_key, load_key},
    record::sqlite_store::SqliteStore,
    record::store::Store,
    settings::Settings,
};
use atuin_common::api::LoginRequest;
use rpassword::prompt_password;

#[derive(Parser, Debug)]
pub struct Cmd {
    #[clap(long, short)]
    pub username: Option<String>,

    #[clap(long, short)]
    pub password: Option<String>,

    /// The encryption key for your account
    #[clap(long, short)]
    pub key: Option<String>,
}

fn get_input() -> Result<String> {
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim_end_matches(&['\r', '\n'][..]).to_string())
}

impl Cmd {
    pub async fn run(&self, settings: &Settings, store: &SqliteStore) -> Result<()> {
        // TODO(ellie): Replace this with a call to atuin_client::login::login
        // The reason I haven't done this yet is that this implementation allows for
        // an empty key. This will use an existing key file.
        //
        // I'd quite like to ditch that behaviour, so have not brought it into the library
        // function.
        if settings.logged_in() {
            bail!(
                "You are already logged in! Please run 'atuin logout' if you wish to login again"
            );
        }

        let username = or_user_input(self.username.clone(), "username");
        let password = self.password.clone().unwrap_or_else(read_user_password);

        let key_path = settings.key_path.as_str();
        let key_path = PathBuf::from(key_path);

        println!("IMPORTANT");
        println!(
            "If you are already logged in on another machine, you must ensure that the key you use here is the same as the key you used there."
        );
        println!("You can find your key by running 'atuin key' on the other machine");
        println!("Do not share this key with anyone");
        println!("\nRead more here: https://docs.atuin.sh/guide/sync/#login \n");

        let key = or_user_input(
            self.key.clone(),
            "encryption key [blank to use existing key file]",
        );

        // if provided, the key may be EITHER base64, or a bip mnemonic
        // try to normalize on base64
        let key = if key.is_empty() {
            key
        } else {
            // try parse the key as a mnemonic...
            match bip39::Mnemonic::from_phrase(&key, bip39::Language::English) {
                Ok(mnemonic) => encode_key(Key::from_slice(mnemonic.entropy()))?,
                Err(err) => {
                    match err.downcast_ref::<bip39::ErrorKind>() {
                        Some(err) => {
                            match err {
                                // assume they copied in the base64 key
                                bip39::ErrorKind::InvalidWord => key,
                                bip39::ErrorKind::InvalidChecksum => {
                                    bail!("key mnemonic was not valid")
                                }
                                bip39::ErrorKind::InvalidKeysize(_)
                                | bip39::ErrorKind::InvalidWordLength(_)
                                | bip39::ErrorKind::InvalidEntropyLength(_, _) => {
                                    bail!("key was not the correct length")
                                }
                            }
                        }
                        _ => {
                            // unknown error. assume they copied the base64 key
                            key
                        }
                    }
                }
            }
        };

        // I've simplified this a little, but it could really do with a refactor
        // Annoyingly, it's also very important to get it correct
        if key.is_empty() {
            if key_path.exists() {
                let bytes = fs_err::read_to_string(key_path)
                    .context("existing key file couldn't be read")?;
                if decode_key(bytes).is_err() {
                    bail!("the key in existing key file was invalid");
                }
            } else {
                panic!(
                    "No key provided. Please use 'atuin key' on your other machine, or recover your key from a backup."
                )
            }
        } else if !key_path.exists() {
            if decode_key(key.clone()).is_err() {
                bail!("the specified key was invalid");
            }

            let mut file = File::create(key_path).await?;
            file.write_all(key.as_bytes()).await?;
        } else {
            // we now know that the user has logged in specifying a key, AND that the key path
            // exists

            // 1. check if the saved key and the provided key match. if so, nothing to do.
            // 2. if not, re-encrypt the local history and overwrite the key
            let current_key: [u8; 32] = load_key(settings)?.into();

            let encoded = key.clone(); // gonna want to save it in a bit
            let new_key: [u8; 32] = decode_key(key)
                .context("could not decode provided key - is not valid base64")?
                .into();

            if new_key != current_key {
                println!("\nRe-encrypting local store with new key");

                store.re_encrypt(&current_key, &new_key).await?;

                println!("Writing new key");
                let mut file = File::create(key_path).await?;
                file.write_all(encoded.as_bytes()).await?;
            }
        }

        let session = api_client::login(
            settings.sync_address.as_str(),
            LoginRequest { username, password },
        )
        .await?;

        let session_path = settings.session_path.as_str();
        let mut file = File::create(session_path).await?;
        file.write_all(session.session.as_bytes()).await?;

        println!("Logged in!");

        Ok(())
    }
}

pub(super) fn or_user_input(value: Option<String>, name: &'static str) -> String {
    value.unwrap_or_else(|| read_user_input(name))
}

pub(super) fn read_user_password() -> String {
    let password = prompt_password("Please enter password: ");
    password.expect("Failed to read from input")
}

fn read_user_input(name: &'static str) -> String {
    eprint!("Please enter {name}: ");
    get_input().expect("Failed to read from input")
}

#[cfg(test)]
mod tests {
    use atuin_client::encryption::Key;

    #[test]
    fn mnemonic_round_trip() {
        let key = Key::from([
            3, 1, 4, 1, 5, 9, 2, 6, 5, 3, 5, 8, 9, 7, 9, 3, 2, 3, 8, 4, 6, 2, 6, 4, 3, 3, 8, 3, 2,
            7, 9, 5,
        ]);
        let phrase = bip39::Mnemonic::from_entropy(&key, bip39::Language::English)
            .unwrap()
            .into_phrase();
        let mnemonic = bip39::Mnemonic::from_phrase(&phrase, bip39::Language::English).unwrap();
        assert_eq!(mnemonic.entropy(), key.as_slice());
        assert_eq!(
            phrase,
            "adapt amused able anxiety mother adapt beef gaze amount else seat alcohol cage lottery avoid scare alcohol cactus school avoid coral adjust catch pink"
        );
    }
}
