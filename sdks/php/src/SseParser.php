<?php
// File: ballista/sdks/php/src/SseParser.php

declare(strict_types=1);

namespace Ballista\Client;

use Ballista\Client\Exception\TransportException;

/** Incremental `text/event-stream` parser. */
final class SseParser
{
    private const MAX_BUFFER = 16 * 1024 * 1024;

    private string $buffer = '';
    private string $event = 'message';
    /** @var list<string> */
    private array $data = [];

    /** @return list<array{event: string, data: string}> */
    public function push(string $chunk): array
    {
        $this->buffer .= $chunk;
        if (strlen($this->buffer) > self::MAX_BUFFER) {
            throw new TransportException('event stream line too long');
        }
        $out = [];
        while (preg_match('/\r\n|\r|\n/', $this->buffer, $m, PREG_OFFSET_CAPTURE) === 1) {
            $pos = $m[0][1];
            $sep = $m[0][0];
            if ($sep === "\r" && $pos + 1 === strlen($this->buffer)) {
                break;
            }
            $line = substr($this->buffer, 0, $pos);
            $this->buffer = (string) substr($this->buffer, $pos + strlen($sep));
            if ($line === '') {
                if ($this->data !== []) {
                    $out[] = ['event' => $this->event, 'data' => implode("\n", $this->data)];
                }
                $this->event = 'message';
                $this->data = [];
                continue;
            }
            if ($line[0] === ':') {
                continue;
            }
            [$field, $value] = array_pad(explode(':', $line, 2), 2, '');
            if (str_starts_with($value, ' ')) {
                $value = substr($value, 1);
            }
            if ($field === 'event') {
                $this->event = $value;
            } elseif ($field === 'data') {
                $this->data[] = $value;
            }
        }
        return $out;
    }
}
