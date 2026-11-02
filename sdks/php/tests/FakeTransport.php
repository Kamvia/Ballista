<?php
// File: ballista/sdks/php/tests/FakeTransport.php

declare(strict_types=1);

namespace Ballista\Client\Tests;

use Ballista\Client\Http\Response;
use Ballista\Client\Http\Transport;

/** Records requests and answers from a handler; streams split the body into small chunks. */
final class FakeTransport implements Transport
{
    /** @var list<array{method: string, url: string, headers: array<string, string>, body: ?string}> */
    public array $requests = [];

    /** @param \Closure(string, string, array<string, string>, ?string): Response $handler */
    public function __construct(private readonly \Closure $handler)
    {
    }

    public function send(string $method, string $url, array $headers, ?string $body, float $timeout): Response
    {
        $this->requests[] = ['method' => $method, 'url' => $url, 'headers' => $headers, 'body' => $body];
        return ($this->handler)($method, $url, $headers, $body);
    }

    public function stream(string $method, string $url, array $headers, callable $onChunk, float $idleTimeout): Response
    {
        $res = $this->send($method, $url, $headers, null, $idleTimeout);
        if ($res->status === 200) {
            foreach (str_split($res->body, 7) as $chunk) {
                if (!$onChunk($chunk)) {
                    break;
                }
            }
        }
        return new Response($res->status, $res->headers, $res->status === 200 ? '' : $res->body);
    }
}
