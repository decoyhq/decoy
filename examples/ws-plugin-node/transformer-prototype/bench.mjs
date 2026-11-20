import assert from 'node:assert/strict'
import { availableParallelism } from 'node:os'
import { decode, encode } from '@msgpack/msgpack'
import { Socket, decodeProto, encodeProto, startDecoy } from '../tests/decoy.ts'

const ROUNDS = Number(process.env.ROUNDS ?? 3)
const SOAK_CLIENTS = 64
const SOAK_PER_CLIENT = 5628
const PROFILE = process.env.PROFILE ?? 'compare'
const COMMAND_SOCKETS = 8
const COMMAND_PER_SOCKET = 128
const SIZES = [
  {
    topic: 'watch',
    type: 'approvals.v1.WatchRequest',
    message: { ids: ['a-1'] },
    state: 'APPROVED',
  },
  { topic: 'lookup', type: 'users.v1.GetUserRequest', message: { id: 'a-1' }, state: 'PENDING' },
]

/** Собирает кадр на стороне клиента независимо от кода transformer. */
function frame({ topic, type, message }) {
  const body = encodeProto(type, message)
  const header = Buffer.alloc(5)
  header.writeUInt32BE(body.length, 1)
  return encode({ meta: { topic }, payload: Buffer.concat([header, body]) })
}

/** Проверяет реальный ответ сокета и возвращает время запроса. */
async function roundTrip(socket, mode, index) {
  const sample = SIZES[index % SIZES.length]
  const request = mode === 'text' ? JSON.stringify(sample.message) : frame(sample)
  const start = performance.now()
  socket.send(request)
  const response = await socket.next(10_000)
  const elapsed = performance.now() - start
  const expected = { id: 'a-1', state: sample.state }

  if (mode === 'text') {
    assert.deepEqual(JSON.parse(response), expected)
  } else {
    assert.ok(response instanceof Uint8Array)
    const { meta, payload } = decode(response)
    assert.deepEqual(meta, { topic: 'approval' })
    const bytes = Buffer.from(payload)
    assert.equal(bytes[0], 0)
    assert.equal(bytes.readUInt32BE(1), bytes.length - 5)
    assert.deepEqual(decodeProto('approvals.v1.Approval', bytes.subarray(5)), expected)
  }
  return elapsed
}

/** Прогревает соединения и измеряет один запрос за раз на каждом сокете. */
async function measure(mode, clients, count) {
  const sockets = await Promise.all(
    Array.from({ length: clients }, (_, i) => Socket.open(`/ws/transformer/${mode}`, `bench-${i}`)),
  )
  try {
    await Promise.all(
      sockets.map(async (socket) => {
        await roundTrip(socket, mode, 0)
        await roundTrip(socket, mode, 1)
      }),
    )
    const start = performance.now()
    const samples = await Promise.all(
      sockets.map(async (socket) => {
        const times = []
        for (let i = 0; i < count; i++) times.push(await roundTrip(socket, mode, i))
        return times
      }),
    )
    return { elapsed: performance.now() - start, samples: samples.flat() }
  } finally {
    for (const socket of sockets) socket.close()
  }
}

/** Вычисляет перцентиль по отсортированным измерениям. */
function percentile(times, p) {
  return times[Math.ceil(times.length * p) - 1].toFixed(2)
}

const started = performance.now()
const stop = await startDecoy()
try {
  console.log(
    JSON.stringify({
      node: process.version,
      platform: process.platform,
      arch: process.arch,
      cores: availableParallelism(),
      profile: PROFILE,
      rounds: ROUNDS,
      healthMs: Math.round(performance.now() - started),
    }),
  )
  const modes =
    PROFILE === 'soak'
      ? ['text', 'sdk', 'sdk-host']
      : PROFILE === 'commands'
        ? ['command', 'split', 'native']
        : ['text', 'sdk', 'sdk-host', 'command', 'split', 'native']
  const workloads =
    PROFILE === 'soak'
      ? [[SOAK_CLIENTS, SOAK_PER_CLIENT]]
      : PROFILE === 'commands'
        ? [[COMMAND_SOCKETS, COMMAND_PER_SOCKET]]
        : [
            [1, 24],
            [8, 8],
          ]
  for (const [clients, count] of workloads) {
    const rows = new Map(modes.map((mode) => [mode, { elapsed: 0, samples: [] }]))
    for (let round = 0; round < ROUNDS; round++) {
      for (let index = 0; index < modes.length; index++) {
        const mode = modes[(index + round) % modes.length]
        const result = await measure(mode, clients, count)
        const row = rows.get(mode)
        row.elapsed += result.elapsed
        for (const elapsed of result.samples) row.samples.push(elapsed)
        console.error(
          `Finished ${mode}, ${clients} sockets, round ${round + 1}: ${row.samples.length} calls`,
        )
      }
    }
    for (const [mode, { elapsed, samples }] of rows) {
      samples.sort((a, b) => a - b)
      console.log(
        JSON.stringify({
          mode,
          clients,
          requests: samples.length,
          p50ms: percentile(samples, 0.5),
          p95ms: percentile(samples, 0.95),
          p99ms: percentile(samples, 0.99),
          requestsPerSecond: Math.round((samples.length * 1000) / elapsed),
        }),
      )
    }
  }
} finally {
  stop()
}
