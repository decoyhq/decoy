import { serve } from './serve'

if (process.env.DECOY_PLUGIN === '1') {
  // stdout carries the protocol: logs go to stderr, from import on, so a `console.log` at the top
  // of a plugin file can't corrupt it.
  console.log = console.info = console.debug = console.error
}

/** Protocol version this SDK speaks (`docs/design/plugin-protocol.md`). */
export const PROTOCOL = 1

/** JSON data, as Cases and Behaviors see it. */
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json }

export interface Connection {
  id: string

  /** Route id. */
  route: string

  session: string
}

export interface Context {
  /** Inbound: the kind of frame being decoded. Outbound and at startup: undefined. */
  frame: 'text' | 'binary' | undefined

  /** Undefined at startup (`check`). */
  connection: Connection | undefined

  /** Kept per connection for as long as it is open; a fresh map at startup. */
  state: Map<string, unknown>

  /** The protos Decoy compiled at startup. */
  protobuf: {
    has(type: string): Promise<boolean>

    /** The message as JSON, mapped the way gRPC Cases see it. */
    decode(type: string, bytes: Uint8Array): Promise<Json>

    /** Unknown fields reject with their field path. */
    encode(type: string, value: unknown): Promise<Uint8Array>
  }
}

/** One pipeline step. */
export interface Step {
  /**
   * Inbound: the frame's bytes → the value the Cases see. Call `miss` to skip the frame. In a
   * pipeline of several steps, a later step gets the previous step's value instead.
   */
  decode(input: Uint8Array, ctx: Context): unknown

  /**
   * Outbound: a rendered message (JSON) → a string (sent as a text frame) or bytes (a binary
   * frame). In a pipeline of several steps, an inner step's result goes to the step before it.
   */
  encode(value: unknown, ctx: Context): unknown

  /** Optional: validate an outbound literal at startup, `${ }` templates included; throw to fail. */
  check?(value: unknown, ctx: Context): unknown
}

export interface Plugin {
  /** Shown in logs and errors as `<key>: <name>@<version>`. */
  name: string

  version: string

  steps: Record<string, Step>
}

/** Thrown by {@link miss}. */
export class Miss extends Error {
  override name = 'Miss'
}

/** From `decode`: this frame matches nothing. Decoy logs `reason` and keeps the socket open. */
export function miss(reason: string): never {
  throw new Miss(reason)
}

/**
 * Defines a plugin and returns it. When Decoy started this process (`DECOY_PLUGIN=1`), also serves
 * it over stdin/stdout, so `node ./plugins/my-plugin.ts` is the whole command. Anywhere else (a
 * unit test importing the file) it only returns the plugin.
 */
export function definePlugin<P extends Plugin>(plugin: P): P {
  if (process.env.DECOY_PLUGIN === '1') serve(plugin)

  return plugin
}
