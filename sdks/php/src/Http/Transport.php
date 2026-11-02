<?php
// File: ballista/sdks/php/src/Http/Transport.php

declare(strict_types=1);

namespace Ballista\Client\Http;

use Ballista\Client\Exception\TransportException;

interface Transport
{
    /**
     * @param array<string, string> $headers
     * @throws TransportException on connection failure
     */
    public function send(string $method, string $url, array $headers, ?string $body, float $timeout): Response;

    /**
     * Streams the response body to `$onChunk`; returning false from it ends the stream.
     * The returned Response holds status and headers (and the body when the status is not 2xx).
     *
     * @param array<string, string> $headers
     * @param callable(string): bool $onChunk
     * @throws TransportException on connection failure or idle timeout
     */
    public function stream(string $method, string $url, array $headers, callable $onChunk, float $idleTimeout): Response;
}
