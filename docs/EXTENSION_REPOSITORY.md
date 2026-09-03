# The extension repository

> **A prebuilt extension is code you download and then run with the full
> privileges of your database process.** `.load` maps a shared object into the
> host and calls its entry point. So a SQL Anywhere release does not just
> publish extension archives, it publishes a signed statement of what those
> archives are.

SQL Anywhere ships loadable extensions as release archives, today the
experimental [cr-sqlite](CRDT.md) CRDT extension, built per platform by
[`.github/workflows/crsqlite.yml`](../.github/workflows/crsqlite.yml). Before
this, installing one meant fetching a `.so` from a release page and hoping:
nothing tied the file you got to the file that was built.

## What a release carries

Alongside the per-target archives, each release carries three small files:

| File | What it is |
|------|-----------|
| `SHA256SUMS` | Plain digests, so `sha256sum -c` gives anyone an integrity check with no special tooling. |
| `MANIFEST.json` | The signed statement: every artifact, its digest and size, and the extension interface version it was compiled against. |
| `MANIFEST.json.sig` | A detached Ed25519 signature over the exact bytes of `MANIFEST.json`. |

```json
{
  "schema": 1,
  "release": "v0.5.3",
  "signing_key": "ece35bde",
  "extensions": [
    {
      "name": "crsqlite",
      "target": "aarch64-apple-darwin",
      "file": "crsqlite-v0.5.3-aarch64-apple-darwin.tar.gz",
      "size": 1214976,
      "sha256": "ab76ed21…",
      "sqlanywhere_api_version": 1
    }
  ]
}
```

The signature covers the file as written rather than a re-serialisation of the
structure, so verification never depends on reproducing the same JSON byte for
byte.

Every extension artifact attached to a release is built by
[`crsqlite.yml`](../.github/workflows/crsqlite.yml) and described by the
manifest. That is a property worth preserving: a second workflow uploading its
own archive on the side would publish something the manifest does not cover, and
therefore something nobody can verify. If you add a workflow that attaches an
extension to a release, extend the sign job to cover it rather than publishing
around it.

`sqlanywhere_api_version` records the value of `SQLANYWHERE_API_VERSION` the
artifact was built against; see
[the extension thunk](../sqlanywhere-sqlite3/doc/sqlanywhere_extensions.md#the-extension-thunk-and-its-version).
A host implementing a lower interface version should refuse the file rather
than load it and discover the mismatch by crashing.

## Verifying a download

Download the archives you want plus all three metadata files into one
directory, then:

```console
$ cargo xtask verify-extensions ./downloads
signature OK (key ece35bde)
  ok  crsqlite-v0.5.3-aarch64-apple-darwin.tar.gz
1 artifact(s) verified
```

This checks, in order: the signature against the committed trust root, then
each present artifact's digest and size, then that the interface version the
artifact needs is one this tree implements. Artifacts listed in the manifest
but absent locally are reported and skipped, so verifying a single-platform
download is normal.

Without the repository checked out, the integrity half still works anywhere:

```console
$ sha256sum -c SHA256SUMS
```

That tells you the bytes match what the manifest says. It does not tell you who
wrote the manifest, which is what the signature is for.

## Keys

The trust root is a committed public key,
`sqlanywhere-sqlite3/ext/SIGNING_KEY.pub`. The private half lives only with the
maintainer and as the CI secret `EXTENSION_SIGNING_KEY`.

Creating it is deliberately a manual, one-time step. CI cannot mint its own
trust root:

```console
$ cargo xtask extension-keygen
key id:      ece35bde
public key:  sqlanywhere-sqlite3/ext/SIGNING_KEY.pub
secret key:  sqlanywhere-sqlite3/ext/SIGNING_KEY.secret  (chmod 600)
```

Then commit `SIGNING_KEY.pub`, store the `sqlanywhere-ext-seckey-v1 …` line as
the `EXTENSION_SIGNING_KEY` repository secret, keep an offline copy, and **do
not commit the secret file**.

Every key carries a short id, the first four bytes of the SHA-256 of the public
key, and both the manifest and the signature name the key that signed. Rotation
therefore does not need a flag day: publish the new public key, and old
releases stay verifiable against the key id they were signed with.

Until the key exists, releases are still built and still get a manifest and
`SHA256SUMS`, but the manifest's `signing_key` is `null` and no `.sig` is
produced. `verify-extensions` treats that as a hard failure unless you pass
`--allow-unsigned`, so an unsigned release cannot quietly pass as a signed one.

## What this does not do yet

**The engine does not enforce any of it.** `.load` and
`sqlite3_load_extension()` will still open any shared object you point them at.
Verification is a step you take before installing, not something the library
does for you.

Making the loader enforce signatures is a policy decision, not just code, and
it needs answers first:

- Where does the loader get the trusted key: compiled in, a config file, a
  keyring? A compiled-in key means a new release to rotate.
- What happens to local development builds and third-party extensions, which by
  definition are not signed by this project? An escape hatch that is easy to
  set is an escape hatch attackers will set.
- DuckDB's answer is a signed repository plus `INSTALL`, so the trust check
  happens at install time and `LOAD` trusts the install directory. That shape
  would fit SQL Anywhere, but it means owning an install path and a cache
  directory rather than a plain file path.

Until that is decided, the honest description is the one above: the project
signs what it publishes, and gives you a way to check it.
