<?php
// File: ballista/sdks/php/src/Client.php

declare(strict_types=1);

namespace Ballista\Client;

use Ballista\Client\Exception\ApiException;
use Ballista\Client\Exception\ConfigurationException;
use Ballista\Client\Exception\RunTimeoutException;
use Ballista\Client\Exception\TransportException;
use Ballista\Client\Http\CurlTransport;
use Ballista\Client\Http\Response;
use Ballista\Client\Http\Transport;
use Ballista\Client\Model\BundleInfo;
use Ballista\Client\Model\Run;
use Ballista\Client\Model\RunEvent;
use Ballista\Client\Model\RunState;
use Ballista\Client\Model\Snapshot;

/** Client for the run manager API. Requests are retried where it is safe to do so. */
final class Client
{
    public const TOKEN_ENV = 'BALLISTA_TOKEN';
    public const URL_ENV = 'BALLISTA_URL';
    public const ARTIFACTS = ['report.json', 'report.html', 'stats.csv', 'history.csv', 'failures.csv'];
    private const USER_AGENT = 'ballista-client-php/0.1.0';
    private const RETRY_STATUSES = [429, 502, 503, 504];

    public readonly string $baseUrl;
    private readonly string $token;
    private readonly Transport $transport;

    /**
     * @param string|null $url server url (or BALLISTA_URL)
     * @param string|null $token API token (or BALLISTA_TOKEN)
     * @param bool $allowInsecureHttp permit plain http to a non-loopback host (the token is sent in clear)
     */
    public function __construct(
        ?string $url = null,
        #[\SensitiveParameter] ?string $token = null,
        private readonly float $timeout = 30.0,
        ?Transport $transport = null,
        private readonly int $attempts = 4,
        private readonly float $baseDelay = 0.5,
        private readonly float $maxDelay = 20.0,
        bool $allowInsecureHttp = false,
        ?string $caFile = null,
    ) {
        $this->baseUrl = self::baseUrl($url ?? (getenv(self::URL_ENV) ?: ''), $allowInsecureHttp);
        $t = trim($token ?? (getenv(self::TOKEN_ENV) ?: ''));
        if (strlen($t) < 32 || preg_match('/\s/', $t) === 1) {
            throw new ConfigurationException('api token required (argument or ' . self::TOKEN_ENV . ')');
        }
        $this->token = $t;
        $this->transport = $transport ?? new CurlTransport($caFile);
    }

    /** @return array<string, string> */
    public function __debugInfo(): array
    {
        return ['baseUrl' => $this->baseUrl];
    }

    private static function isLoopback(string $host): bool
    {
        $h = trim($host, '[]');
        return $h === 'localhost' || $h === '::1' || str_starts_with($h, '127.');
    }

    private static function baseUrl(string $raw, bool $allowInsecure): string
    {
        $raw = trim($raw);
        $p = parse_url($raw);
        if ($raw === '' || $p === false || !isset($p['scheme'], $p['host']) || isset($p['user']) || isset($p['pass'])
            || !in_array(strtolower($p['scheme']), ['http', 'https'], true)) {
            throw new ConfigurationException('server url must be http(s)://host[:port] without credentials (or ' . self::URL_ENV . ')');
        }
        $scheme = strtolower($p['scheme']);
        if ($scheme === 'http' && !self::isLoopback($p['host']) && !$allowInsecure) {
            throw new ConfigurationException('refusing to send the token over plain http to a remote host; use https');
        }
        $port = isset($p['port']) ? ':' . $p['port'] : '';
        return "{$scheme}://{$p['host']}{$port}/api/v1";
    }

    /** @return array<string, string> */
    private function headers(array $extra = []): array
    {
        return ['authorization' => "Bearer {$this->token}", 'user-agent' => self::USER_AGENT, 'accept' => 'application/json'] + $extra;
    }

    private function delay(int $attempt, ?float $retryAfter): float
    {
        if ($retryAfter !== null) {
            return min(max($retryAfter, 0.0), $this->maxDelay);
        }
        $backoff = min($this->baseDelay * (2 ** $attempt), $this->maxDelay);
        return $backoff * (0.5 + mt_rand() / mt_getrandmax() / 2);
    }

    private static function retryAfter(Response $r): ?float
    {
        $v = $r->header('retry-after');
        return $v !== null && is_numeric($v) ? (float) $v : null;
    }

    private static function raiseFor(Response $r): void
    {
        if ($r->status >= 200 && $r->status < 300) {
            return;
        }
        $code = 'error';
        $detail = 'request failed';
        $body = json_decode($r->body, true);
        if (is_array($body) && is_array($body['error'] ?? null)) {
            $code = is_string($body['error']['code'] ?? null) ? $body['error']['code'] : $code;
            $detail = is_string($body['error']['message'] ?? null) ? $body['error']['message'] : $detail;
        }
        throw ApiException::forStatus($r->status, $code, substr($detail, 0, 1000), self::retryAfter($r));
    }

    /**
     * @param array<string, string> $query
     * @param array<string, string> $headers
     */
    private function request(string $method, string $path, ?array $body = null, array $headers = [], array $query = []): Response
    {
        $url = $this->baseUrl . $path . ($query ? '?' . http_build_query($query, '', '&', PHP_QUERY_RFC3986) : '');
        $h = $this->headers($headers);
        $payload = null;
        if ($body !== null) {
            $payload = json_encode($body, JSON_THROW_ON_ERROR | JSON_UNESCAPED_SLASHES | JSON_PRESERVE_ZERO_FRACTION);
            $h['content-type'] = 'application/json';
        }
        $attempts = max(1, $this->attempts);
        for ($attempt = 0; $attempt < $attempts; $attempt++) {
            try {
                $res = $this->transport->send($method, $url, $h, $payload, $this->timeout);
            } catch (TransportException $e) {
                if ($attempt + 1 >= $attempts) {
                    throw $e;
                }
                usleep((int) ($this->delay($attempt, null) * 1e6));
                continue;
            }
            if (in_array($res->status, self::RETRY_STATUSES, true) && $attempt + 1 < $attempts) {
                usleep((int) ($this->delay($attempt, self::retryAfter($res)) * 1e6));
                continue;
            }
            self::raiseFor($res);
            return $res;
        }
        throw new TransportException("{$method} {$path}: retries exhausted");
    }

    private function json(string $method, string $path, ?array $body = null, array $headers = [], array $query = []): mixed
    {
        $res = $this->request($method, $path, $body, $headers, $query);
        try {
            return json_decode($res->body, true, 64, JSON_THROW_ON_ERROR);
        } catch (\JsonException) {
            throw new TransportException('server returned invalid JSON');
        }
    }

    /** @return array{status: string, version: string} */
    public function health(): array
    {
        /** @var array{status: string, version: string} */
        return $this->json('GET', '/health');
    }

    /** @return array{name: string, scope: string} */
    public function whoami(): array
    {
        /** @var array{name: string, scope: string} */
        return $this->json('GET', '/whoami');
    }

    /** @return array{version: string, active_run: ?string, queued: int, limits: array{max_users: float, max_run_time_secs: int, max_queue: int}} */
    public function status(): array
    {
        /** @var array{version: string, active_run: ?string, queued: int, limits: array{max_users: float, max_run_time_secs: int, max_queue: int}} */
        return $this->json('GET', '/status');
    }

    /**
     * Validates a plan (as it would run with `$overrides`) without storing it.
     *
     * @param Plan|array<string, mixed>|string $plan
     * @param array<string, mixed> $overrides
     * @return array{name: string, scenarios: list<string>, files: list<string>, document: array<string, mixed>}
     */
    public function validatePlan(Plan|array|string $plan, array $overrides = []): array
    {
        $body = ['plan' => Bundle::planPayload($plan)];
        if ($overrides !== []) {
            $body['overrides'] = (object) Validate::overrides($overrides);
        }
        $out = $this->json('POST', '/plans/validate', $body);
        if (!is_array($out) || !is_array($out['files'] ?? null) || !is_array($out['scenarios'] ?? null)) {
            throw new TransportException('unexpected response: plan check');
        }
        /** @var array{name: string, scenarios: list<string>, files: list<string>, document: array<string, mixed>} */
        return $out;
    }

    /** Uploads a plan (with assets); identical content returns the stored bundle. Safe to retry. */
    public function uploadBundle(Bundle|Plan $bundle): BundleInfo
    {
        $b = $bundle instanceof Bundle ? $bundle : new Bundle($bundle);
        return BundleInfo::fromArray($this->json('POST', '/bundles', $b->toPayload()));
    }

    /** @return list<BundleInfo> */
    public function listBundles(): array
    {
        $page = $this->json('GET', '/bundles');
        return array_map(BundleInfo::fromArray(...), is_array($page) ? (array) ($page['items'] ?? []) : []);
    }

    public function getBundle(string $id): BundleInfo
    {
        return BundleInfo::fromArray($this->json('GET', '/bundles/' . Validate::id($id, Validate::BUNDLE_ID, 'bundle')));
    }

    public function deleteBundle(string $id): void
    {
        $this->request('DELETE', '/bundles/' . Validate::id($id, Validate::BUNDLE_ID, 'bundle'));
    }

    /**
     * Queues a run. A generated idempotency key makes retries safe against duplicate runs.
     *
     * @param array<string, mixed> $overrides users, spawn_rate, run_time, iterations, tags, exclude_tags, host
     * @param array<string, string> $labels
     * @param array<string, mixed> $regression baseline, max_regression_pct, metrics, min_samples, min_change_ms, max_error_rate_increase
     */
    public function createRun(string|BundleInfo $bundle, array $overrides = [], array $labels = [], ?string $idempotencyKey = null, array $regression = []): Run
    {
        $id = Validate::id($bundle instanceof BundleInfo ? $bundle->id : $bundle, Validate::BUNDLE_ID, 'bundle');
        $key = $idempotencyKey ?? bin2hex(random_bytes(16));
        if (preg_match('/^[\x21-\x7e]{1,128}$/', $key) !== 1) {
            throw new ConfigurationException('idempotency key must be 1..128 printable characters');
        }
        $body = [
            'bundle' => $id,
            'overrides' => (object) Validate::overrides($overrides),
            'labels' => (object) Validate::labels($labels),
        ];
        if ($regression !== []) {
            $body['regression'] = (object) Validate::regression($regression);
        }
        return Run::fromArray($this->json('POST', '/runs', $body, ['idempotency-key' => $key]));
    }

    /** @return array{items: list<Run>, next_cursor: ?string} */
    public function listRuns(?RunState $state = null, int $limit = 50, ?string $cursor = null): array
    {
        $query = ['limit' => (string) max(1, min($limit, 200))];
        if ($state !== null) {
            $query['state'] = $state->value;
        }
        if ($cursor !== null) {
            $query['cursor'] = $cursor;
        }
        $page = $this->json('GET', '/runs', null, [], $query);
        $items = is_array($page) ? (array) ($page['items'] ?? []) : [];
        $next = is_array($page) && is_string($page['next_cursor'] ?? null) ? $page['next_cursor'] : null;
        return ['items' => array_map(Run::fromArray(...), array_values($items)), 'next_cursor' => $next];
    }

    /** @return \Generator<int, Run> */
    public function iterRuns(?RunState $state = null, int $pageSize = 100): \Generator
    {
        $cursor = null;
        do {
            $page = $this->listRuns($state, $pageSize, $cursor);
            yield from $page['items'];
            $cursor = $page['next_cursor'];
        } while ($cursor !== null);
    }

    public function getRun(string $id): Run
    {
        return Run::fromArray($this->json('GET', '/runs/' . Validate::id($id, Validate::RUN_ID, 'run')));
    }

    public function stopRun(string $id): Run
    {
        return Run::fromArray($this->json('POST', '/runs/' . Validate::id($id, Validate::RUN_ID, 'run') . '/stop'));
    }

    public function deleteRun(string $id): void
    {
        $this->request('DELETE', '/runs/' . Validate::id($id, Validate::RUN_ID, 'run'));
    }

    public function getStats(string $id): Snapshot
    {
        return Snapshot::fromArray($this->json('GET', '/runs/' . Validate::id($id, Validate::RUN_ID, 'run') . '/stats'));
    }

    /**
     * Compares a finished run with a baseline (default: the latest earlier passed run of the same plan).
     *
     * @param list<string> $metrics
     * @return array{baseline: ?string, passed: bool, regressions: int, changes: list<array<string, mixed>>, unmatched: list<string>, note: ?string}
     */
    public function compare(string $id, string $baseline = 'auto', ?float $maxRegressionPct = null, array $metrics = []): array
    {
        $query = ['baseline' => $baseline];
        if ($maxRegressionPct !== null) {
            $query['max_regression_pct'] = (string) $maxRegressionPct;
        }
        if ($metrics !== []) {
            $query['metrics'] = implode(',', $metrics);
        }
        $out = $this->json('GET', '/runs/' . Validate::id($id, Validate::RUN_ID, 'run') . '/compare', null, [], $query);
        if (!is_array($out) || !isset($out['passed'], $out['regressions'])) {
            throw new TransportException('unexpected response: comparison');
        }
        /** @var array{baseline: ?string, passed: bool, regressions: int, changes: list<array<string, mixed>>, unmatched: list<string>, note: ?string} */
        return $out;
    }

    public function artifact(string $id, string $name): string
    {
        if (!in_array($name, self::ARTIFACTS, true)) {
            throw new ConfigurationException("unknown artifact '{$name}'");
        }
        return $this->request('GET', '/runs/' . Validate::id($id, Validate::RUN_ID, 'run') . '/artifacts/' . $name)->body;
    }

    /** @return array<string, mixed> */
    public function report(string $id): array
    {
        $r = json_decode($this->artifact($id, 'report.json'), true, 512, JSON_THROW_ON_ERROR);
        return is_array($r) ? $r : [];
    }

    /** Writes an artifact to `$dest` (a file path, or an existing directory to place it in). */
    public function download(string $id, string $name, string $dest): string
    {
        $target = is_dir($dest) ? rtrim($dest, '/\\') . DIRECTORY_SEPARATOR . "{$id}-{$name}" : $dest;
        $data = $this->artifact($id, $name);
        $tmp = $target . '.part';
        if (file_put_contents($tmp, $data, LOCK_EX) === false || !rename($tmp, $target)) {
            throw new TransportException("cannot write {$target}");
        }
        return $target;
    }

    /**
     * Streams run events until the run finishes (`end` is the last event).
     * `$onEvent` may return false to stop early.
     *
     * @param callable(RunEvent): (bool|null) $onEvent
     */
    public function events(string $id, callable $onEvent, float $idleTimeout = 60.0): void
    {
        $path = '/runs/' . Validate::id($id, Validate::RUN_ID, 'run') . '/events';
        $parser = new SseParser();
        $ended = false;
        $res = $this->transport->stream('GET', $this->baseUrl . $path, $this->headers(['accept' => 'text/event-stream']), function (string $chunk) use ($parser, $onEvent, &$ended): bool {
            foreach ($parser->push($chunk) as $msg) {
                $data = json_decode($msg['data'], true);
                $ev = match ($msg['event']) {
                    'stats' => new RunEvent('stats', stats: Snapshot::fromArray($data)),
                    'run', 'end' => new RunEvent($msg['event'], run: Run::fromArray($data)),
                    default => null,
                };
                if ($ev === null) {
                    continue;
                }
                if ($onEvent($ev) === false || $ev->type === 'end') {
                    $ended = true;
                    return false;
                }
            }
            return true;
        }, $idleTimeout);
        self::raiseFor($res);
        if (!$ended) {
            throw new TransportException('event stream closed before the run finished');
        }
    }

    /**
     * Blocks until the run finishes. Uses the event stream and falls back to polling.
     *
     * @param (callable(Snapshot): void)|null $onStats
     * @param (callable(Run): void)|null $onState
     */
    public function wait(string $id, ?float $timeout = null, ?callable $onStats = null, ?callable $onState = null, float $pollInterval = 2.0): Run
    {
        $deadline = $timeout === null ? null : microtime(true) + $timeout;
        $final = null;
        while (true) {
            try {
                $this->events($id, function (RunEvent $ev) use ($onStats, $onState, $deadline, &$final): bool {
                    if ($deadline !== null && microtime(true) > $deadline) {
                        return false;
                    }
                    if ($ev->stats !== null && $onStats !== null) {
                        $onStats($ev->stats);
                    }
                    if ($ev->run !== null) {
                        if ($onState !== null) {
                            $onState($ev->run);
                        }
                        if ($ev->type === 'end') {
                            $final = $ev->run;
                        }
                    }
                    return true;
                }, max($pollInterval * 10, 30.0));
            } catch (TransportException) {
                // fall back to polling below
            } catch (ApiException $e) {
                if (!in_array($e->status, self::RETRY_STATUSES, true)) {
                    throw $e;
                }
            }
            if ($final instanceof Run) {
                return $final;
            }
            $run = $this->getRun($id);
            if ($run->isFinished()) {
                return $run;
            }
            if ($deadline !== null && microtime(true) > $deadline) {
                throw new RunTimeoutException("run {$id} did not finish within {$timeout}s");
            }
            usleep((int) ($pollInterval * 1e6));
        }
    }

    /**
     * Uploads (when given a Plan or Bundle), queues and optionally waits for a run.
     *
     * @param array<string, mixed> $overrides
     * @param array<string, string> $labels
     * @param (callable(Snapshot): void)|null $onStats
     */
    public function run(Plan|Bundle|BundleInfo|string $bundle, array $overrides = [], array $labels = [], bool $wait = true, ?float $timeout = null, ?callable $onStats = null, array $regression = []): Run
    {
        $target = $bundle instanceof Bundle || $bundle instanceof Plan ? $this->uploadBundle($bundle) : $bundle;
        $run = $this->createRun($target, $overrides, $labels, null, $regression);
        return $wait ? $this->wait($run->id, $timeout, $onStats) : $run;
    }
}
