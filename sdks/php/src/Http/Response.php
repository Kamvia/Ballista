<?php
// File: ballista/sdks/php/src/Http/Response.php

declare(strict_types=1);

namespace Ballista\Client\Http;

final readonly class Response
{
    /** @param array<string, string> $headers lower-case names */
    public function __construct(public int $status, public array $headers, public string $body)
    {
    }

    public function header(string $name): ?string
    {
        return $this->headers[strtolower($name)] ?? null;
    }
}
