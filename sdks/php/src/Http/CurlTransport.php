<?php
// File: ballista/sdks/php/src/Http/CurlTransport.php

declare(strict_types=1);

namespace Ballista\Client\Http;

use Ballista\Client\Exception\TransportException;

/** HTTP over ext-curl with TLS verification always on; redirects are never followed. */
final class CurlTransport implements Transport
{
    private const MAX_BODY = 256 * 1024 * 1024;

    /** @param string|null $caFile PEM bundle for private certificate authorities */
    public function __construct(private readonly ?string $caFile = null, private readonly float $connectTimeout = 10.0)
    {
        if ($caFile !== null && !is_file($caFile)) {
            throw new TransportException('CA bundle not found');
        }
    }

    /**
     * @param array<string, string> $headers
     * @return array{0: \CurlHandle, 1: \ArrayObject<string, string>}
     */
    private function handle(string $method, string $url, array $headers): array
    {
        $ch = curl_init();
        $collected = new \ArrayObject();
        $lines = [];
        foreach ($headers as $k => $v) {
            if (preg_match('/[\r\n]/', $k . $v) === 1) {
                throw new TransportException('header values must not contain line breaks');
            }
            $lines[] = "{$k}: {$v}";
        }
        curl_setopt_array($ch, [
            CURLOPT_URL => $url,
            CURLOPT_CUSTOMREQUEST => $method,
            CURLOPT_HTTPHEADER => $lines,
            CURLOPT_FOLLOWLOCATION => false,
            CURLOPT_PROTOCOLS => CURLPROTO_HTTP | CURLPROTO_HTTPS,
            CURLOPT_SSL_VERIFYPEER => true,
            CURLOPT_SSL_VERIFYHOST => 2,
            CURLOPT_CONNECTTIMEOUT_MS => (int) ($this->connectTimeout * 1000),
            CURLOPT_HEADERFUNCTION => static function ($h, string $line) use ($collected): int {
                $parts = explode(':', $line, 2);
                if (count($parts) === 2) {
                    $collected[strtolower(trim($parts[0]))] = trim($parts[1]);
                }
                return strlen($line);
            },
        ]);
        if ($this->caFile !== null) {
            curl_setopt($ch, CURLOPT_CAINFO, $this->caFile);
        }
        return [$ch, $collected];
    }

    public function send(string $method, string $url, array $headers, ?string $body, float $timeout): Response
    {
        [$ch, $hdrs] = $this->handle($method, $url, $headers);
        $buffer = '';
        curl_setopt_array($ch, [
            CURLOPT_TIMEOUT_MS => (int) ($timeout * 1000),
            CURLOPT_WRITEFUNCTION => static function ($h, string $chunk) use (&$buffer): int {
                if (strlen($buffer) + strlen($chunk) > self::MAX_BODY) {
                    return 0;
                }
                $buffer .= $chunk;
                return strlen($chunk);
            },
        ]);
        if ($body !== null) {
            curl_setopt($ch, CURLOPT_POSTFIELDS, $body);
        }
        if (curl_exec($ch) === false) {
            $err = curl_errno($ch);
            curl_close($ch);
            throw new TransportException("{$method} request failed (curl error {$err})");
        }
        $status = (int) curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
        curl_close($ch);
        return new Response($status, $hdrs->getArrayCopy(), $buffer);
    }

    public function stream(string $method, string $url, array $headers, callable $onChunk, float $idleTimeout): Response
    {
        [$ch, $hdrs] = $this->handle($method, $url, $headers);
        $errorBody = '';
        $stopped = false;
        curl_setopt_array($ch, [
            CURLOPT_TIMEOUT_MS => 0,
            CURLOPT_LOW_SPEED_LIMIT => 1,
            CURLOPT_LOW_SPEED_TIME => max(1, (int) ceil($idleTimeout)),
            CURLOPT_WRITEFUNCTION => static function ($h, string $chunk) use ($onChunk, &$errorBody, &$stopped): int {
                $status = (int) curl_getinfo($h, CURLINFO_RESPONSE_CODE);
                if ($status < 200 || $status >= 300) {
                    $errorBody .= substr($chunk, 0, 65536);
                    return strlen($chunk);
                }
                if (!$onChunk($chunk)) {
                    $stopped = true;
                    return 0;
                }
                return strlen($chunk);
            },
        ]);
        $ok = curl_exec($ch);
        $status = (int) curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
        $err = curl_errno($ch);
        curl_close($ch);
        if ($ok === false && !$stopped) {
            throw new TransportException("event stream interrupted (curl error {$err})");
        }
        return new Response($status, $hdrs->getArrayCopy(), $errorBody);
    }
}
