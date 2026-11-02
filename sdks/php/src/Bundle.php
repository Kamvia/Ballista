<?php
// File: ballista/sdks/php/src/Bundle.php

declare(strict_types=1);

namespace Ballista\Client;

use Ballista\Client\Exception\ConfigurationException;

/**
 * A plan and optional assets (descriptor sets, certificates, upload files) sent as content.
 * Inline scripts (`source`) and inline feeder `rows` need no files.
 */
final class Bundle
{
    public const MAX_FILES = 256;
    public const MAX_FILE_BYTES = 16 * 1024 * 1024;
    public const MAX_TOTAL_BYTES = 64 * 1024 * 1024;
    public const MAX_PLAN_BYTES = 1024 * 1024;
    private const SEGMENT = '/^[A-Za-z0-9_-][A-Za-z0-9._-]{0,99}$/';
    private const RESERVED = ['plan.toml', 'bundle.json'];

    /** @var array<string, mixed>|string */
    public readonly array|string $plan;
    /** @var array<string, string> */
    public readonly array $labels;
    /** @var array<string, string> */
    private array $files = [];

    /**
     * @param Plan|array<string, mixed>|string $plan structured plan, raw plan document, or plan file text
     * @param array<string, string> $labels
     */
    public function __construct(Plan|array|string $plan, array $labels = [])
    {
        $this->plan = self::planPayload($plan);
        $this->labels = Validate::labels($labels);
    }

    /**
     * The `plan` field sent to the server.
     *
     * @param Plan|array<string, mixed>|string $plan
     * @return array<string, mixed>|string
     */
    public static function planPayload(Plan|array|string $plan): array|string
    {
        if ($plan instanceof Plan) {
            return $plan->toDocument();
        }
        if (is_string($plan)) {
            if (trim($plan) === '' || strlen($plan) > self::MAX_PLAN_BYTES || preg_match('//u', $plan) !== 1) {
                throw new ConfigurationException('plan text must be non-empty UTF-8 of at most 1 MiB');
            }
            return $plan;
        }
        if ($plan === [] || array_is_list($plan)) {
            throw new ConfigurationException('a plan document must be an associative array');
        }
        return $plan;
    }

    /** Mirrors the server rule: relative, at most 8 segments, no hidden or parent segments. */
    public static function validPath(string $rel): bool
    {
        if ($rel === '' || strlen($rel) > 200 || str_starts_with($rel, '/')) {
            return false;
        }
        $parts = explode('/', $rel);
        if (count($parts) > 8 || $parts[0] === 'inline') {
            return false;
        }
        foreach ($parts as $p) {
            if (preg_match(self::SEGMENT, $p) !== 1) {
                return false;
            }
        }
        return true;
    }

    public function addBytes(string $rel, string $data): self
    {
        if (!self::validPath($rel) || in_array($rel, self::RESERVED, true)) {
            throw new ConfigurationException("invalid bundle path '{$rel}'");
        }
        if (strlen($data) > self::MAX_FILE_BYTES) {
            throw new ConfigurationException("'{$rel}' exceeds 16 MiB");
        }
        if (!isset($this->files[$rel]) && count($this->files) >= self::MAX_FILES) {
            throw new ConfigurationException('a bundle holds at most ' . self::MAX_FILES . ' files');
        }
        $total = strlen($data);
        foreach ($this->files as $k => $v) {
            if ($k !== $rel) {
                $total += strlen($v);
            }
        }
        if ($total > self::MAX_TOTAL_BYTES) {
            throw new ConfigurationException('bundle exceeds 64 MiB');
        }
        $this->files[$rel] = $data;
        ksort($this->files, SORT_STRING);
        return $this;
    }

    /** Adds the content of a local file under the bundle path `$rel`. */
    public function addPath(string $rel, string $path): self
    {
        if (!is_file($path)) {
            throw new ConfigurationException("'{$rel}': not a regular file");
        }
        if ((int) filesize($path) > self::MAX_FILE_BYTES) {
            throw new ConfigurationException("'{$rel}' exceeds 16 MiB");
        }
        return $this->addBytes($rel, (string) file_get_contents($path));
    }

    /** @return array<string, int> relative path to size in bytes */
    public function files(): array
    {
        return array_map('strlen', $this->files);
    }

    /** @return array{plan: array<string, mixed>|string, files: array<string, string>|object, labels: array<string, string>|object} */
    public function toPayload(): array
    {
        return [
            'plan' => $this->plan,
            'files' => $this->files === [] ? new \stdClass() : array_map('base64_encode', $this->files),
            'labels' => $this->labels === [] ? new \stdClass() : $this->labels,
        ];
    }
}
