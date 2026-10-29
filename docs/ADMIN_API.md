# Sqlanywhere-server admin API documentation

This document describes the admin API endpoints.

The admin API is used to manage namespaces on a `sqld` instance. Namespaces are isolated database within a same sqld instance.

To enable the admin API, and manage namespaces, two extra flags need to be passed to `sqld`:

- `--admin-listen-addr <addr>:<port>`: the address and port on which the admin API should listen. It must be different from the user API listen address (which defaults to port 8080).
- `--enable-namespaces`: enable namespaces for the instance. By default namespaces are disabled.

## Routes

```HTTP
POST /v1/namespaces/:namespace/create
```

Create a namespace named `:namespace`.
body:

```json
{
    "dump_url"?: string,
}
```

```HTTP
DELETE /v1/namespaces/:namespace
```

Delete the namespace named `:namespace`.

```HTTP
POST /v1/namespaces/:namespace/fork/:to
```

Fork `:namespace` into new namespace `:to`. With a body of
`{"timestamp": "2026-10-01T12:00:00"}` (UTC), the fork is restored from the
bottomless backup as of that moment; that needs bottomless replication.

```HTTP
POST /v1/namespaces/:namespace/at
```

Make `:namespace` as it was at a moment available as a read-only namespace
that other namespaces can `ATTACH`, restored from the bottomless backup. Body:
`{"timestamp": "2026-10-01T12:00:00"}` (UTC). Responds with the namespace's
name, `{"namespace": "orders@20261001T120000Z"}`; asking for the same moment
again returns the same one. With `"ttl_seconds": 3600` the primary deletes it,
with its backups, an hour after making it; otherwise delete it like any other
namespace when done. See
[TIME_TRAVEL.md](TIME_TRAVEL.md).

```HTTP
POST /v1/namespaces/:namespace/branch/:branch
```

Branch `:namespace` for a three-way merge: makes `:branch@base`, the namespace
as it is now, read-only, and `:branch`, writable, forked from that base so
both start from the same state. Both allow `ATTACH`, so the live namespace can
compare and merge the branch (`sqlanywhere::merge3`); other settings are the
source's. Responds `{"branch": ":branch", "base": ":branch@base"}`. Delete
both namespaces when done. See
[BRANCHES.md](BRANCHES.md#branches-of-a-sqld-namespace).
