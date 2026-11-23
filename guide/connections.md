# Connections

The **Connections** screen lists saved profiles on the left and the connection form on the right. Fill in the form, then **Test**, **Save** or **Connect**. Several connections can be open at once, each in its own tab.

## General

| Field | Meaning |
|---|---|
| Name, Colour tag | How the profile appears in the list and the tab. A red tag on production servers helps. |
| Host, Port, User, Password | The MariaDB account. Default port: 3306. |
| Default database | Optional. Selected after connecting. |
| Unix socket | Optional. Replaces host and port (macOS and Linux). |
| Save passwords in the system keychain | Off: the password is asked at each connection. |

When a password is already saved, the field shows **Saved in the keychain**: leave it empty to keep it.

Deleting a profile also deletes its saved passwords.

## Encryption (TLS)

| Mode | What it does |
|---|---|
| Disabled | Clear text. Only for local servers. |
| Preferred | Encrypts when the server supports it, without checking its certificate. |
| Required | Always encrypts, fails if the server has no TLS. The certificate is not checked. |
| Verify CA | Encrypts and checks the certificate is signed by a trusted CA. |
| Verify CA and host name | Most secure: also checks the certificate matches the host name. |

For **Verify CA** with a private CA, give the **CA certificate** (PEM file). If the server requires a client certificate (`REQUIRE X509`), also give the **Client certificate** and **Client key**.

## SSH tunnel

Tick **Connect through an SSH tunnel** to reach a server that only listens on a private network.

- **SSH server**: the bastion, as `host` or `host:port`, and its user.
- **Private key**: for example `~/.ssh/id_ed25519`. Leave empty to log in with the SSH password. A passphrase can be saved in the keychain.
- **SSH certificate**: optional. If `<key>-cert.pub` exists next to the key, it is used automatically.
- **Host** and **Port** in the General section are then seen from the bastion (often `127.0.0.1:3306`).

At the first connection, Otaria shows the bastion's host key fingerprint. Check it, then tick **I checked this fingerprint, trust it and retry**. Host keys signed by an SSH certificate authority are also accepted when the CA is trusted.

## After connecting

The server tab shows the server version, the logged-in account, the connection ID, and whether the link is encrypted (with the cipher) or goes through SSH. On a MySQL server, a notice says that MariaDB-specific features are hidden.
