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
  /** Тип кадра для входящего сообщения WebSocket. */
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

export type Plugin = {
  /** Имя и версия отображаются в диагностике рядом с именем Transformer. */
  name: string

  version: string

  /** Преобразует входящее значение. Чтобы пропустить сообщение, вызови `miss`. */
  input?(value: unknown, ctx: Context): unknown

  /** Преобразует отрисованный payload или результат предыдущего шага. */
  output?(value: unknown, ctx: Context): unknown

  /** Проверяет литерал при старте, включая строки с шаблонами. */
  check?(value: unknown, ctx: Context): unknown
}

/** Thrown by {@link miss}. */
export class Miss extends Error {
  override name = 'Miss'
}

/** From `input`: this frame matches nothing. Decoy logs `reason` and keeps the socket open. */
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
