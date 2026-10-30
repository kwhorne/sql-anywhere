# Observability

`sqld` exports Prometheus metrics, and `tools/observability` has a Grafana
dashboard for them, with Prometheus and Grafana provisioned to run beside a
server in one command.

## Turning metrics on

Metrics are served at `/metrics` on the **admin API**, which is off until it
is given an address:

```console
sqld --db-path data.sqld --admin-listen-addr 127.0.0.1:9090
curl http://127.0.0.1:9090/metrics
```

In the Docker image, set `SQLD_ADMIN_LISTEN_ADDR=0.0.0.0:9090`. The admin API
also creates, forks and deletes namespaces, so keep it on a private network
or give it `--admin-auth-key`; never publish it next to the user API.
`--disable-metrics` keeps the admin API and drops `/metrics`.

Set `SQLD_APP_LABEL` to add an `app` label (and a `version` label) to every
series, which tells several deployments apart in one Prometheus.

## The dashboard

```console
docker compose -f tools/observability/docker-compose.yml up
```

starts `sqld` (user API on <http://localhost:8080>), Prometheus scraping it,
and Grafana on <http://localhost:3000> with the **SQL Anywhere sqld**
dashboard already loaded. To use the dashboard with your own Grafana, import
[`tools/observability/grafana/dashboards/sqld.json`](../tools/observability/grafana/dashboards/sqld.json)
and pick a Prometheus data source; [`prometheus.yml`](../tools/observability/prometheus.yml)
is the scrape configuration.

It has four rows, filtered by `instance` and `namespace`:

- **Server**: servers up, open connections, read and write statements per
  second, statement execution time (p50, p99), HTTP responses by status,
  failed statements by error code, bytes returned.
- **Namespaces**: queries per second, mean query time, rows read and written,
  database size, replication index.
- **Storage engine**: WAL checkpoints and their p99 time, connection setup
  time, statement memory.
- **Runtime**: the tokio runtime's threads and queues.

Every metric the dashboard uses is checked against a real `sqld`'s `/metrics`
by `sqlanywhere-server/tests/observability.rs`, which also fails when the
server exports a metric this page does not describe.

## Reading the numbers

- **Histograms are summaries.** The exporter writes each histogram as a
  Prometheus summary: `name{quantile="0.5"}` … `name{quantile="0.99"}`,
  `name_sum` and `name_count`, over a sliding window. Quantiles from several
  instances cannot be averaged, so the dashboard shows the worst instance
  (`max`). `rate(name_sum) / rate(name_count)` is a mean that does aggregate.
- **Durations are in seconds**, except `sqlanywhere_server_query_latency`,
  which counts milliseconds.
- **Rows read** include every DiskANN node a vector search visits, so a
  vector-heavy namespace reads more rows than its result sizes suggest (see
  [VECTOR_SEARCH.md](VECTOR_SEARCH.md#6-looking-inside-an-index)).
- Series appear the first time something records them: a server that has
  never failed a statement has no `sqlanywhere_server_hrana_step_errors`
  series. Series idle for two minutes are dropped.

## Metric reference

### Statements and queries

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `sqlanywhere_server_reads_count` | counter | | Read statements executed. |
| `sqlanywhere_server_writes_count` | counter | | Write statements executed. |
| `sqlanywhere_server_statement_execution_time` | summary (s) | | Time to execute one statement. |
| `sqlanywhere_server_statement_mem_used_bytes` | summary (bytes) | | Memory used by a prepared statement. |
| `sqlanywhere_server_returned_bytes` | summary (bytes) | | Bytes of values returned to the client per statement. |
| `sqlanywhere_server_query_count` | counter | `namespace` (and a series without) | Queries executed. |
| `sqlanywhere_server_query_latency` | counter (ms) | `namespace` (and a series without) | Total query time. |
| `sqlanywhere_server_query_rows_read` | counter | | Rows read by queries, all namespaces. |
| `sqlanywhere_server_query_rows_written` | counter | | Rows written by queries, all namespaces. |
| `sqlanywhere_server_query_mem_used` | counter (bytes) | | Memory used by queries, summed. |
| `sqlanywhere_server_query_canceled` | counter | | Queries stopped because their request was canceled. |
| `sqlanywhere_server_hrana_step_errors` | counter | `code` | Statements that failed, by Hrana error code. |
| `sqlanywhere_server_sqlanywhere_execute_program` | counter | | Hrana programs (batches) executed. |
| `sqlanywhere_server_sqlanywhere_query_execute` | counter | | Statements executed within programs. |
| `sqlanywhere_server_describe_count` | counter | | `describe` calls. |
| `sqlanywhere_server_total_response_size_before_lock` | summary (bytes) | | Bytes of responses in flight, sampled each time a connection is created. |

### Namespaces

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `sqlanywhere_server_rows_read` | counter | `namespace` | Rows read. |
| `sqlanywhere_server_rows_written` | counter | `namespace` | Rows written. |
| `sqlanywhere_server_storage` | gauge (bytes) | `namespace` | Database size, excluding frames not yet checkpointed. |
| `sqlanywhere_server_current_frame_no` | gauge | `namespace` | Last frame number written: the replication index. |
| `sqlanywhere_server_namespace_load_latency` | summary (s) | | Time to load a namespace that was not in memory. |
| `sqlanywhere_server_write_requests_delegated` | counter | `namespace` | Writes a replica forwarded to its primary. |
| `sqlanywhere_server_embedded_replica_frames_replicated` | counter | `namespace` | Frames sent to embedded replicas. |

### Connections and HTTP

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `sqlanywhere_server_concurrent_connections` | gauge | | Database connections open now. |
| `sqlanywhere_server_connection_create_time` | summary (s) | | Time to open a database connection. |
| `sqlanywhere_server_connection_alive_duration` | summary (s) | | How long a connection stayed open. |
| `sqlanywhere_server_write_txn_duration` | summary (s) | | How long a write transaction stayed open. |
| `sqlanywhere_server_stream_handles` | gauge | | Hrana stream handles held in memory. |
| `sqlanywhere_server_user_http_response` | counter | `protocol`, `status` | User API responses, by protocol (`http`, `grpc`) and status. |
| `sqlanywhere_server_user_http_fault` | counter | `protocol` | User API responses that were server errors. |
| `sqlanywhere_server_legacy_http_call` | counter | | Calls to the legacy HTTP API. |
| `sqlanywhere_server_requests_proxied` | counter | | Requests a replica proxied to its primary. |
| `sqlanywhere_client_version` | counter | `version` | Requests by client library version. |

### Storage engine

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `sqlanywhere_server_wal_checkpoint_count` | counter | | WAL checkpoints. |
| `sqlanywhere_server_wal_checkpoint_time` | summary (s) | | Time to checkpoint the WAL. |
| `sqlanywhere_server_vacuum_count` | counter | | Vacuum operations. |
| `sqlanywhere_server_dirty_startup` | counter | | Declared for starts that found the replication log dirty; not recorded by this version. |

### Replication and live queries

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `sqlanywhere_server_replication_latency` | summary (s) | | On a replica: time from a commit on the primary to its frame arriving. |
| `sqlanywhere_server_replication_latency_out_of_sync` | counter | | Latency samples dropped because the clocks disagreed. |
| `sqlanywhere_server_replication_latencies_cache_misses` | counter | | Frames that arrived without the primary's commit time, so no latency could be measured. |
| `sqlanywhere_server_replica_exec` | counter | | Programs a replica ran locally. |
| `sqlanywhere_server_replica_exec_mispredict` | counter | | Programs a replica ran locally, found to be an interactive transaction, and sent to the primary instead. |
| `sqlanywhere_server_listen_events_sent` | counter | | Commit notifications sent to `/beta/listen` subscribers. |
| `sqlanywhere_server_listen_events_dropped` | counter | | Commit notifications dropped for a subscriber that fell behind. |

### Process and runtime

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `sqlanywhere_server_count` | gauge | | 1 per running server: a liveness signal. |
| `tokio_runtime_num_workers` | gauge | | Worker threads. |
| `tokio_runtime_num_blocking_threads` | gauge | | Blocking threads. |
| `tokio_runtime_num_idle_blocking_threads` | gauge | | Idle blocking threads. |
| `tokio_runtime_blocking_queue_depth` | gauge | | Tasks waiting for a blocking thread. |
| `tokio_runtime_injection_queue_depth` | gauge | | Tasks scheduled from outside the runtime, waiting. |
| `tokio_runtime_io_driver_fd_registered_count` | counter | | File descriptors registered with the I/O driver. |
| `tokio_runtime_io_driver_fd_deregistered_count` | counter | | File descriptors deregistered. |
| `tokio_runtime_io_driver_ready_count` | counter | | Readiness events from the I/O driver. |
| `tokio_runtime_remote_schedule_count` | counter | | Tasks scheduled from outside the runtime. |
