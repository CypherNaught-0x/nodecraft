# Backup file format

A NodeCraft backup (#182) is one file holding named entries (projects, media, library items,
settings) with a manifest and integrity checks, optionally encrypted. Encryption is
recommended: an unencrypted backup can be read by anyone who has the file. A lost passphrase
can't be recovered; there is no reset. `nodecraft_format::backup` reads and writes the
container. Choosing what goes in, staging a restore and publishing it atomically are the host's
work.

## Layout (version 1)

All integers are little-endian.

| Bytes | Field |
|---|---|
| 8 | magic `NCBACKUP` |
| 2 | version, `1` |
| 1 | flags: `0` unencrypted, `1` encrypted (any other value is refused) |
| *encrypted only:* 1 | key derivation: `1` = Argon2id v0x13 |
| 4 + 4 + 4 | Argon2id memory (KiB), passes, lanes |
| 16 | salt (random per backup) |
| 19 | nonce prefix (random per backup; never reuse a salt and prefix pair with the same passphrase) |

Then the **payload**: a 4-byte manifest length, the manifest as JSON, and every entry's bytes in
manifest order.

- **Unencrypted:** the payload follows the header as is, then a 32-byte BLAKE3 hash of the
  header and payload together. This catches truncation and accidental corruption. The hash has no
  key, though, so someone who edits the file on purpose can recompute it. Only encryption detects
  deliberate edits.
- **Encrypted:** the payload is split into 64 KiB chunks. Each chunk is sealed with
  XChaCha20-Poly1305 and stored as ciphertext plus a 16-byte tag. Each nonce is the 19-byte
  prefix, a big-endian 32-bit chunk counter and a final byte, `1` for the last chunk and `0`
  otherwise. The header bytes are the associated data of every chunk. The writer never makes an
  empty last chunk, but readers accept one. The key is Argon2id(passphrase, salt) with the
  header's parameters, 32 bytes.

## Manifest

```json
{"label": "Weekly", "created_ms": 1760000000000,
 "entries": [{"path": "projects/brief.ncraft", "size": 13, "blake3": "<64 lowercase hex>"}]}
```

Unknown fields are refused.

**Paths** are relative and use `/` between components. Each component may contain ASCII
letters, digits, `.`, `-`, `_` and inner spaces. A component may not be `.` or `..`, may not end
in `.`, and may not start or end with a space. A path is at most 255 bytes and 16 components deep.
Windows device names (`con`, `prn`, `aux`, `nul`, `com0`–`com9`, `lpt0`–`lpt9`, with or without
an extension, and with spaces before it) are refused. Paths are unique ignoring case, and no path may also be a folder of
another entry. So no entry
can escape its restore folder or collide on a case-insensitive disk.

## Checks

| Check | Limit |
|---|---|
| entries | 100,000 |
| one entry | 64 GiB |
| all entries | 1 TiB |
| manifest | 32 MiB |
| Argon2id read bounds | 19–256 MiB memory, 1–6 passes, 1–8 lanes (refused before deriving) |
| Argon2id for new backups | 64 MiB, 3 passes, 1 lane |

**Writing** checks every entry's bytes against the manifest's size and hash as they stream. A
source that changed after it was listed is an error (`Conflict`).

**Reading** reports an entry complete only after its hash matches, and the backup complete only
after the end checks pass:
- for an encrypted backup, the last chunk ends exactly where the manifest says, with nothing
  after it;
- for an unencrypted backup, the trailer hash matches and nothing follows it.

For an encrypted backup, a wrong passphrase, tampering and truncation read as `Authentication`.
For an unencrypted one, a bad trailer or entry hash reads as `Authentication`, and truncation
reads as `InvalidInput`. Extra data and unsafe paths read as `InvalidInput`, and a newer version
or option as `Unsupported`. A passphrase given for an unencrypted backup is refused
(`Authentication`): an encrypted backup may have been swapped for a crafted plain one. A writer
whose entry failed refuses further entries and `finish`, so no incomplete backup looks complete. Hosts write
restored entries to a staging folder and publish them only after `finish` succeeds, so a failed
restore leaves existing work untouched.

## Secrets

Passphrases and derived keys are never stored or included in errors. The derived key and the
cipher's key state are zeroed when dropped; Argon2id's working memory and decrypted buffers are
freed but not wiped. Key derivation takes
seconds on purpose, so hosts run it off the UI thread. Passphrases are used as typed; they are not
Unicode-normalized. Provider credentials and keychain entries are never offered as backup entries. After a
restore, connections need their keys entered again.
