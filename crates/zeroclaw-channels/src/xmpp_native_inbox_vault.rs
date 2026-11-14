//! Shared existing-vault envelope for channel and offline lab administration.
use anyhow::{Context, ensure};
pub struct VaultInboxCipher {
    pub store: zeroclaw_config::secrets::SecretStore,
    pub alias: String,
}
impl crate::xmpp_omemo::inbox::InboxCipher for VaultInboxCipher {
    fn check_existing(&self) -> anyhow::Result<()> {
        self.store
            .keyed_digest(b"native-omemo-inbox-existing-vault-v1", b"")?;
        Ok(())
    }
    fn seal(&self, plaintext: &str) -> anyhow::Result<String> {
        self.store
            .keyed_digest(b"native-omemo-inbox-existing-vault-v1", b"")?;
        let value = serde_json::json!({"domain":"native-omemo-ingress-v1","alias":self.alias,"body":plaintext});
        self.store.encrypt(&serde_json::to_string(&value)?)
    }
    fn open(&self, ciphertext: &str) -> anyhow::Result<String> {
        ensure!(
            ciphertext.starts_with("enc2:"),
            "Inbox must use authenticated vault encryption"
        );
        let decrypted = self.store.decrypt(ciphertext)?;
        let value: serde_json::Value = serde_json::from_str(&decrypted)?;
        ensure!(
            value["domain"] == "native-omemo-ingress-v1" && value["alias"] == self.alias,
            "Inbox channel binding mismatch"
        );
        Ok(value["body"]
            .as_str()
            .context("Missing inbox content")?
            .into())
    }
}
