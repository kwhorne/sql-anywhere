<?php

// The generated PHP contract API, called the way a runtime calls it, against a
// real SQLite database through PDO. Mirrors sqlanywhere/tests/contract_api.rs.
// Run: php clients/php/tests/contracts_test.php   (no dependencies)

declare(strict_types=1);

use SqlAnywhere\Contracts\{Cache, PubSub, Queue};

spl_autoload_register(function (string $class): void {
    $prefix = 'SqlAnywhere\\Contracts\\';
    if (str_starts_with($class, $prefix)) {
        require __DIR__ . '/../src/' . substr($class, strlen($prefix)) . '.php';
    }
});

$failures = 0;
function check(bool $ok, string $what): void
{
    global $failures;
    if (!$ok) {
        $failures++;
        $at = debug_backtrace()[0]['line'];
        fwrite(STDERR, "FAIL line $at: $what\n");
    }
}

function db(): PDO
{
    $db = new PDO('sqlite::memory:');
    Queue::install($db);
    Cache::install($db);
    PubSub::install($db);
    // Installing twice is harmless: every schema statement is IF NOT EXISTS.
    Queue::install($db);
    return $db;
}

function count_of(PDO $db, string $table): int
{
    return (int) $db->query("SELECT count(*) FROM $table")->fetchColumn();
}

// --- queue ---
$db = db();
check(Queue::VERSION === 1, 'queue version');
$id = Queue::enqueue($db, 'mail', 'job-1', 0, 0, 3)['id'];
check(Queue::backlog($db, 'mail')['ready'] === 1, 'one ready job');
$claimed = Queue::claim($db, 30, 'mail');
check($claimed !== null && $claimed['id'] === $id && $claimed['payload'] === 'job-1', 'claim returns the job');
check(Queue::claim($db, 30, 'mail') === null, 'nothing else to claim');
check(Queue::renew($db, 60, $id) === 1, 'renew');
check(Queue::release($db, 0, $id) === 1, 'release');
check(Queue::claim($db, 30, 'mail') !== null, 'claimable again');
check(Queue::ack($db, $id) === 1, 'ack');
check(count_of($db, 'askr_jobs') === 0, 'acked job is gone');

// The dead-letter move is two statements in one IMMEDIATE transaction.
$id = Queue::enqueue($db, 'mail', 'job-2', 0, 0, 1)['id'];
check(Queue::deadLetter($db, 'uuid-2', 'boom', $id) === 2, 'one row copied, one deleted');
check(count_of($db, 'askr_jobs') === 0, 'dead-lettered job left the queue');
$failed = $db->query('SELECT uuid, exception FROM askr_failed_jobs')->fetch(PDO::FETCH_ASSOC);
check($failed === ['uuid' => 'uuid-2', 'exception' => 'boom'], 'failed job recorded');
// Nullable parameters bind NULL.
$id = Queue::enqueue($db, 'mail', 'job-3', 0, 0, 1)['id'];
Queue::deadLetter($db, null, null, $id);
check(count_of($db, 'askr_failed_jobs WHERE uuid IS NULL AND exception IS NULL') === 1, 'NULL binds');
// A payload is bytes: NUL and high bytes survive.
$bytes = "a\x00b\xff";
Queue::enqueue($db, 'bin', $bytes, 0, 0, 1);
check(Queue::claim($db, 30, 'bin')['payload'] === $bytes, 'binary payload round-trips');
check($db->query('SELECT typeof(payload) FROM askr_jobs WHERE queue = \'bin\'')->fetchColumn() === 'blob', 'payload is stored as a blob');
// The dead-letter move is all or nothing: when its second statement fails,
// the first is rolled back, and no transaction is left open.
$id = Queue::enqueue($db, 'mail', 'job-4', 0, 0, 1)['id'];
$before = count_of($db, 'askr_failed_jobs');
$db->exec("CREATE TEMP TRIGGER refuse BEFORE DELETE ON askr_jobs BEGIN SELECT RAISE(ABORT, 'refused'); END");
try {
    Queue::deadLetter($db, 'uuid-4', 'boom', $id);
    check(false, 'the failing move throws');
} catch (PDOException $e) {
    check(str_contains($e->getMessage(), 'refused'), 'the trigger refused it');
}
$db->exec('DROP TRIGGER refuse');
check(count_of($db, 'askr_failed_jobs') === $before, 'the copy was rolled back');
check(count_of($db, "askr_jobs WHERE id = $id") === 1, 'the job is still queued');
$db->exec('BEGIN');
$db->exec('COMMIT');
check(true, 'no transaction was left open');

// --- cache ---
$db = db();
check(Cache::set($db, 'a', '1', null) === 1, 'set');
check(Cache::get($db, 'a')['value'] === '1', 'get');
check(Cache::get($db, 'missing') === null, 'miss');
// An entry that has expired is not served.
Cache::set($db, 'old', 'x', 1);
check(Cache::get($db, 'old') === null, 'expired is not served');
check(Cache::sweep($db) === 1, 'sweep removes it');
check(Cache::increment($db, 'hits', 5, null)['value'] === 5, 'increment from nothing');
check(Cache::increment($db, 'hits', -2, null)['value'] === 3, 'increment by a negative');
// A lock is held when a row comes back, and only its owner releases it.
check(Cache::lock($db, 'lock:x', 'me', 60) !== null, 'lock taken');
check(Cache::lock($db, 'lock:x', 'you', 60) === null, 'lock held');
check(Cache::unlock($db, 'lock:x', 'you') === 0, 'only the owner unlocks');
check(Cache::unlock($db, 'lock:x', 'me') === 1, 'owner unlocks');
// Invalidating a tag is two deletes in one transaction.
Cache::set($db, 't1', 'v', null);
Cache::set($db, 't2', 'v', null);
Cache::tag($db, 'users', 't1');
Cache::tag($db, 'users', 't2');
check(Cache::invalidateTag($db, 'users') === 4, 'two entries and two tag rows');
check(Cache::get($db, 't1') === null && Cache::get($db, 'a') !== null, 'only tagged entries go');
check(Cache::forget($db, 'a') === 1, 'forget');
check(Cache::flush($db) >= 1 && count_of($db, 'askr_cache') === 0, 'flush');

// --- pub/sub ---
$db = db();
$first = PubSub::publish($db, 'news', 'hello')['seq'];
PubSub::publish($db, 'news', 'world');
PubSub::publish($db, 'other', 'x');
$events = PubSub::subscribe($db, 'news', 0, 10);
check(array_column($events, 'payload') === ['hello', 'world'], 'subscribe reads the channel in order');
check(count(PubSub::subscribe($db, 'news', $first, 10)) === 1, 'subscribe resumes after a cursor');
check(PubSub::saveCursor($db, 'worker', $first) === 1, 'save cursor');
check(PubSub::trimToLast($db, 1) === 2, 'trim to the last event');

if ($failures > 0) {
    fwrite(STDERR, "$failures check(s) failed\n");
    exit(1);
}
echo "php contract API: all checks passed\n";
