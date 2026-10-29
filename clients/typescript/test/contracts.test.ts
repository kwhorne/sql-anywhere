// The generated TypeScript contract API, called the way a runtime calls it,
// on both drivers it ships with. Mirrors sqlanywhere/tests/contract_api.rs and
// clients/php/tests/contracts_test.php.
// Run: npm test

import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { after, describe, test } from "node:test";

import { createClient } from "@libsql/client";

import { cache, fromLibsql, fromNodeSqlite, pubsub, queue, type Driver } from "../src/index.ts";

const dir = mkdtempSync(join(tmpdir(), "sqlanywhere-contracts-"));
after(() => rmSync(dir, { recursive: true, force: true }));

let files = 0;
const drivers: Record<string, () => Driver> = {
  "node:sqlite": () => fromNodeSqlite(new DatabaseSync(join(dir, `node-${files++}.db`))),
  "@libsql/client": () => fromLibsql(createClient({ url: `file:${join(dir, `libsql-${files++}.db`)}` })),
};

// With SQLD_URL set (a running sqld, e.g. http://127.0.0.1:8080), the same
// checks run over HTTP. Each test starts from an empty database.
const sqld = process.env.SQLD_URL;
if (sqld) {
  drivers["sqld"] = () => {
    const client = createClient({ url: sqld });
    const db = fromLibsql(client);
    let fresh = false;
    return {
      ...db,
      async execute(statement) {
        if (!fresh) {
          fresh = true;
          const { rows } = await client.execute(
            "SELECT type, name FROM sqlite_schema WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%'",
          );
          for (const row of rows) {
            await client.execute(`DROP ${row.type} IF EXISTS "${row.name}"`);
          }
        }
        return db.execute(statement);
      },
    };
  };
}

const text = (value: unknown) => new TextDecoder().decode(value as Uint8Array);

async function count(db: Driver, from: string): Promise<number> {
  const { rows } = await db.execute({ sql: `SELECT count(*) AS n FROM ${from}`, args: {} });
  return Number(rows[0]!.n);
}

async function installed(make: () => Driver): Promise<Driver> {
  const db = make();
  await queue.install(db);
  await cache.install(db);
  await pubsub.install(db);
  // Installing twice is harmless: every schema statement is IF NOT EXISTS.
  await queue.install(db);
  return db;
}

for (const [name, make] of Object.entries(drivers)) {
  describe(name, () => {
    test("queue", async () => {
      const db = await installed(make);
      assert.equal(queue.VERSION, 1);
      const { id } = await queue.enqueue(db, { queue: "mail", payload: "job-1", priority: 0, delay: 0, maxAttempts: 3 });
      assert.equal((await queue.backlog(db, { queue: "mail" })).ready, 1);
      const claimed = await queue.claim(db, { visibility: 30, queue: "mail" });
      assert.equal(claimed?.id, id);
      assert.equal(text(claimed?.payload), "job-1");
      assert.equal(await queue.claim(db, { visibility: 30, queue: "mail" }), null, "nothing else to claim");
      assert.equal(await queue.renew(db, { visibility: 60, id: Number(id) }), 1);
      assert.equal(await queue.release(db, { backoff: 0, id: Number(id) }), 1);
      assert.notEqual(await queue.claim(db, { visibility: 30, queue: "mail" }), null, "claimable again");
      assert.equal(await queue.ack(db, { id: Number(id) }), 1);
      assert.equal(await count(db, "askr_jobs"), 0);

      // The dead-letter move is two statements in one IMMEDIATE transaction.
      const second = Number((await queue.enqueue(db, { queue: "mail", payload: "job-2", priority: 0, delay: 0, maxAttempts: 1 })).id);
      assert.equal(await queue.deadLetter(db, { uuid: "uuid-2", exception: "boom", id: second }), 2);
      assert.equal(await count(db, "askr_jobs"), 0);
      const { rows } = await db.execute({ sql: "SELECT uuid, exception FROM askr_failed_jobs", args: {} });
      assert.deepEqual({ ...rows[0] }, { uuid: "uuid-2", exception: "boom" });
      // Nullable arguments may be left out, and bind NULL.
      const third = Number((await queue.enqueue(db, { queue: "mail", payload: "job-3", priority: 0, delay: 0, maxAttempts: 1 })).id);
      await queue.deadLetter(db, { id: third });
      assert.equal(await count(db, "askr_failed_jobs WHERE uuid IS NULL AND exception IS NULL"), 1);

      // A payload is bytes: NUL and high bytes survive, stored as a blob.
      const bytes = new Uint8Array([0x61, 0x00, 0x62, 0xff]);
      await queue.enqueue(db, { queue: "bin", payload: bytes, priority: 0, delay: 0, maxAttempts: 1 });
      const binary = await queue.claim(db, { visibility: 30, queue: "bin" });
      assert.ok(binary?.payload instanceof Uint8Array);
      assert.deepEqual([...(binary.payload as Uint8Array)], [...bytes]);
      const { rows: types } = await db.execute({
        sql: "SELECT typeof(payload) AS t FROM askr_jobs WHERE queue = 'bin'",
        args: {},
      });
      assert.equal(types[0]!.t, "blob");

      // The move is all or nothing: when its second statement fails, the
      // first is rolled back, and no transaction is left open.
      const fourth = Number((await queue.enqueue(db, { queue: "mail", payload: "job-4", priority: 0, delay: 0, maxAttempts: 1 })).id);
      const before = await count(db, "askr_failed_jobs");
      await db.execute({
        sql: "CREATE TRIGGER refuse BEFORE DELETE ON askr_jobs BEGIN SELECT RAISE(ABORT, 'refused'); END",
        args: {},
      });
      await assert.rejects(queue.deadLetter(db, { uuid: "uuid-4", exception: "boom", id: fourth }), /refused/);
      await db.execute({ sql: "DROP TRIGGER refuse", args: {} });
      assert.equal(await count(db, "askr_failed_jobs"), before, "the copy was rolled back");
      assert.equal(await count(db, `askr_jobs WHERE id = ${fourth}`), 1, "the job is still queued");
      await db.batch([{ sql: "SELECT 1", args: {} }], "immediate");
    });

    test("cache", async () => {
      const db = await installed(make);
      assert.equal(await cache.set(db, { key: "a", value: "1" }), 1);
      assert.equal(text((await cache.get(db, { key: "a" }))?.value), "1");
      assert.equal(await cache.get(db, { key: "missing" }), null);
      // An entry that has expired is not served.
      await cache.set(db, { key: "old", value: "x", expiresAt: 1 });
      assert.equal(await cache.get(db, { key: "old" }), null);
      assert.equal(await cache.sweep(db), 1);
      assert.equal((await cache.increment(db, { key: "hits", delta: 5 })).value, 5);
      assert.equal((await cache.increment(db, { key: "hits", delta: -2 })).value, 3);
      // A lock is held when a row comes back, and only its owner releases it.
      assert.notEqual(await cache.lock(db, { key: "lock:x", owner: "me", ttl: 60 }), null);
      assert.equal(await cache.lock(db, { key: "lock:x", owner: "you", ttl: 60 }), null);
      assert.equal(await cache.unlock(db, { key: "lock:x", owner: "you" }), 0);
      assert.equal(await cache.unlock(db, { key: "lock:x", owner: "me" }), 1);
      // Invalidating a tag is two deletes in one transaction.
      await cache.set(db, { key: "t1", value: "v" });
      await cache.set(db, { key: "t2", value: "v" });
      await cache.tag(db, { tag: "users", key: "t1" });
      await cache.tag(db, { tag: "users", key: "t2" });
      assert.equal(await cache.invalidateTag(db, { tag: "users" }), 4);
      assert.equal(await cache.get(db, { key: "t1" }), null);
      assert.notEqual(await cache.get(db, { key: "a" }), null);
      assert.equal(await cache.forget(db, { key: "a" }), 1);
      assert.ok((await cache.flush(db)) >= 1);
      assert.equal(await count(db, "askr_cache"), 0);
    });

    test("pub/sub", async () => {
      const db = await installed(make);
      const first = Number((await pubsub.publish(db, { channel: "news", payload: "hello" })).seq);
      await pubsub.publish(db, { channel: "news", payload: "world" });
      await pubsub.publish(db, { channel: "other", payload: "x" });
      const events = await pubsub.subscribe(db, { channel: "news", cursor: 0, batch: 10 });
      assert.deepEqual(events.map((e) => text(e.payload)), ["hello", "world"]);
      assert.equal((await pubsub.subscribe(db, { channel: "news", cursor: first, batch: 10 })).length, 1);
      assert.equal(await pubsub.saveCursor(db, { name: "worker", cursor: first }), 1);
      assert.equal(await pubsub.trimToLast(db, { keepLast: 1 }), 2);
    });
  });
}
