import { readFileSync } from 'node:fs'

// Runs between the built-in protobuf steps, so stdin and stdout are JSON.
const message = JSON.parse(readFileSync(0, 'utf8')) as { id: string }
message.id = process.argv[2] === 'input' ? message.id.toLowerCase() : `ref-${message.id}`
process.stdout.write(JSON.stringify(message))
