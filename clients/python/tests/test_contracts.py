"""The generated Python contract API, called the way a runtime calls it, on
both drivers it ships with. Mirrors sqlanywhere/tests/contract_api.rs,
clients/php/tests/contracts_test.php and clients/typescript/test.

Run: python -m unittest discover -s tests
With SQLD_URL set (a running sqld, e.g. http://127.0.0.1:8080), the same
checks run over HTTP too; each test starts from an empty database.
"""

from __future__ import annotations

import os
import sqlite3
import tempfile
import unittest

from sqlanywhere_contracts import (
    Driver,
    SqldError,
    Statement,
    cache,
    from_sqld,
    from_sqlite3,
    pubsub,
    queue,
)


def count(db: Driver, source: str) -> int:
    return int(db.execute(Statement(f"SELECT count(*) AS n FROM {source}", {})).rows[0]["n"])


class Base:
    # Nested, so that discovery does not run the checks without a driver.
    class Contracts(unittest.TestCase):
        """The checks, for every driver: a subclass says how to make one."""

        def make(self) -> Driver:
            raise NotImplementedError

        def installed(self) -> Driver:
            db = self.make()
            queue.install(db)
            cache.install(db)
            pubsub.install(db)
            # Installing twice is harmless: every schema statement is IF NOT EXISTS.
            queue.install(db)
            return db

        def test_queue(self) -> None:
            db = self.installed()
            self.assertEqual(queue.VERSION, 1)
            job = queue.enqueue(db, queue="mail", payload="job-1", priority=0, delay=0, max_attempts=3)
            self.assertEqual(queue.backlog(db, queue="mail")["ready"], 1)
            claimed = queue.claim(db, visibility=30, queue="mail")
            self.assertEqual(claimed["id"], job["id"])
            self.assertEqual(claimed["payload"], b"job-1")
            self.assertIsNone(queue.claim(db, visibility=30, queue="mail"), "nothing else to claim")
            self.assertEqual(queue.renew(db, visibility=60, id=job["id"]), 1)
            self.assertEqual(queue.release(db, backoff=0, id=job["id"]), 1)
            self.assertIsNotNone(queue.claim(db, visibility=30, queue="mail"), "claimable again")
            self.assertEqual(queue.ack(db, id=job["id"]), 1)
            self.assertEqual(count(db, "askr_jobs"), 0)

            # The dead-letter move is two statements in one IMMEDIATE transaction.
            second = queue.enqueue(db, queue="mail", payload="job-2", priority=0, delay=0, max_attempts=1)["id"]
            self.assertEqual(queue.dead_letter(db, uuid="uuid-2", exception="boom", id=second), 2)
            self.assertEqual(count(db, "askr_jobs"), 0)
            rows = db.execute(Statement("SELECT uuid, exception FROM askr_failed_jobs", {})).rows
            self.assertEqual(rows[0], {"uuid": "uuid-2", "exception": "boom"})
            # Nullable arguments may be left out, and bind NULL.
            third = queue.enqueue(db, queue="mail", payload="job-3", priority=0, delay=0, max_attempts=1)["id"]
            queue.dead_letter(db, id=third)
            self.assertEqual(count(db, "askr_failed_jobs WHERE uuid IS NULL AND exception IS NULL"), 1)

            # A payload is bytes: NUL and high bytes survive, stored as a blob.
            raw = bytes([0x61, 0x00, 0x62, 0xFF])
            queue.enqueue(db, queue="bin", payload=raw, priority=0, delay=0, max_attempts=1)
            binary = queue.claim(db, visibility=30, queue="bin")
            self.assertEqual(binary["payload"], raw)
            types = db.execute(Statement("SELECT typeof(payload) AS t FROM askr_jobs WHERE queue = 'bin'", {})).rows
            self.assertEqual(types[0]["t"], "blob")

            # The move is all or nothing: when its second statement fails, the
            # first is rolled back, and no transaction is left open.
            fourth = queue.enqueue(db, queue="mail", payload="job-4", priority=0, delay=0, max_attempts=1)["id"]
            before = count(db, "askr_failed_jobs")
            db.execute(
                Statement(
                    "CREATE TRIGGER refuse BEFORE DELETE ON askr_jobs BEGIN SELECT RAISE(ABORT, 'refused'); END",
                    {},
                )
            )
            with self.assertRaisesRegex(Exception, "refused"):
                queue.dead_letter(db, uuid="uuid-4", exception="boom", id=fourth)
            db.execute(Statement("DROP TRIGGER refuse", {}))
            self.assertEqual(count(db, "askr_failed_jobs"), before, "the copy was rolled back")
            self.assertEqual(count(db, f"askr_jobs WHERE id = {fourth}"), 1, "the job is still queued")

            # And when the first statement fails, the second does not run: the job
            # is not deleted without its copy.
            db.execute(
                Statement(
                    "CREATE TRIGGER refuse BEFORE INSERT ON askr_failed_jobs BEGIN SELECT RAISE(ABORT, 'refused'); END",
                    {},
                )
            )
            with self.assertRaisesRegex(Exception, "refused"):
                queue.dead_letter(db, uuid="uuid-4", exception="boom", id=fourth)
            db.execute(Statement("DROP TRIGGER refuse", {}))
            self.assertEqual(count(db, "askr_failed_jobs"), before)
            self.assertEqual(count(db, f"askr_jobs WHERE id = {fourth}"), 1, "the job was not deleted")
            db.batch([Statement("SELECT 1", {})], "immediate")

        def test_cache(self) -> None:
            db = self.installed()
            self.assertEqual(cache.set(db, key="a", value="1"), 1)
            self.assertEqual(cache.get(db, key="a")["value"], b"1")
            self.assertIsNone(cache.get(db, key="missing"))
            # An entry that has expired is not served.
            cache.set(db, key="old", value="x", expires_at=1)
            self.assertIsNone(cache.get(db, key="old"))
            self.assertEqual(cache.sweep(db), 1)
            self.assertEqual(cache.increment(db, key="hits", delta=5)["value"], 5)
            self.assertEqual(cache.increment(db, key="hits", delta=-2)["value"], 3)
            # A lock is held when a row comes back, and only its owner releases it.
            self.assertIsNotNone(cache.lock(db, key="lock:x", owner="me", ttl=60))
            self.assertIsNone(cache.lock(db, key="lock:x", owner="you", ttl=60))
            self.assertEqual(cache.unlock(db, key="lock:x", owner="you"), 0)
            self.assertEqual(cache.unlock(db, key="lock:x", owner="me"), 1)
            # Invalidating a tag is two deletes in one transaction.
            cache.set(db, key="t1", value="v")
            cache.set(db, key="t2", value="v")
            cache.tag(db, tag="users", key="t1")
            cache.tag(db, tag="users", key="t2")
            self.assertEqual(cache.invalidate_tag(db, tag="users"), 4)
            self.assertIsNone(cache.get(db, key="t1"))
            self.assertIsNotNone(cache.get(db, key="a"))
            self.assertEqual(cache.forget(db, key="a"), 1)
            self.assertGreaterEqual(cache.flush(db), 1)
            self.assertEqual(count(db, "askr_cache"), 0)

        def test_pubsub(self) -> None:
            db = self.installed()
            first = pubsub.publish(db, channel="news", payload="hello")["seq"]
            pubsub.publish(db, channel="news", payload="world")
            pubsub.publish(db, channel="other", payload="x")
            events = pubsub.subscribe(db, channel="news", cursor=0, batch=10)
            self.assertEqual([e["payload"] for e in events], [b"hello", b"world"])
            self.assertEqual(len(pubsub.subscribe(db, channel="news", cursor=first, batch=10)), 1)
            self.assertEqual(pubsub.save_cursor(db, name="worker", cursor=first), 1)
            self.assertEqual(pubsub.trim_to_last(db, keep_last=1), 2)


class Sqlite3(Base.Contracts):
    def setUp(self) -> None:
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.files = 0

    def make(self) -> Driver:
        self.files += 1
        conn = sqlite3.connect(os.path.join(self.dir.name, f"{self.files}.db"), isolation_level=None)
        self.addCleanup(conn.close)
        return from_sqlite3(conn)

    def test_a_connection_that_opens_its_own_transactions_is_refused(self) -> None:
        conn = sqlite3.connect(":memory:")
        self.addCleanup(conn.close)
        with self.assertRaisesRegex(ValueError, "isolation_level=None"):
            from_sqlite3(conn)


@unittest.skipUnless(os.environ.get("SQLD_URL"), "SQLD_URL is not set")
class Sqld(Base.Contracts):
    def make(self) -> Driver:
        db = from_sqld(os.environ["SQLD_URL"])
        objects = db.execute(
            Statement(
                "SELECT type, name FROM sqlite_schema "
                "WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%'",
                {},
            )
        ).rows
        for o in objects:
            db.execute(Statement(f'DROP {o["type"]} IF EXISTS "{o["name"]}"', {}))
        return db

    def test_errors_carry_the_code(self) -> None:
        db = self.make()
        with self.assertRaises(SqldError) as caught:
            db.execute(Statement("SELECT no_such_column", {}))
        self.assertIn("no_such_column", str(caught.exception))
        self.assertIsNotNone(caught.exception.code)


if __name__ == "__main__":
    unittest.main()
