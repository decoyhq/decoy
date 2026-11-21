import { serve } from './serve'

if (process.env.DECOY_PLUGIN === '1') {
  // stdout carries the protocol: logs go to stderr, from import on, so a `console.log` at the top
  // of a plugin file can't corrupt it.
  console.log = console.info = console.debug = console.error
}

/** Protocol version this SDK speaks (`docs/reference/plugin-protocol.md`). */
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
  /** Frame type of the incoming WebSocket message. */
  frame: 'text' | 'binary' | undefined

  /** Set on WebSocket input and output calls. */
  connection: Connection | undefined

  /** Shared across calls on a WebSocket connection. Other calls receive a fresh map. */
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

export interface Plugin {
  /** Diagnostics show the worker name and version alongside the Transformer name. */
  name: string

  version: string

  /** Transforms an incoming value. Call `miss` to skip the message. */
  input?: (value: unknown, ctx: Context) => unknown

  /** Transforms the rendered payload or the previous step’s result. */
  output?: (value: unknown, ctx: Context) => unknown

  /** Validates a literal at startup, including strings that contain templates. */
  check?: (value: unknown, ctx: Context) => unknown
}

/** Thrown by {@link miss}. */
export class Miss extends Error {
  override name = 'Miss'
}

/** Signals an input miss. On WebSockets, Decoy logs `reason` and keeps the socket open. */
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
