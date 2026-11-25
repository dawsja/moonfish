export type SshConfigFields = {
  alias: string
  hostname: string
  user: string
  port: string
  identityFile: string
}

const directives = {
  hostname: "HostName",
  user: "User",
  port: "Port",
  identityFile: "IdentityFile",
} as const

type Field = keyof typeof directives
const fields = Object.keys(directives) as Field[]
const aliasPattern = /^[a-z0-9_][a-z0-9_.-]*$/i

export type SshConfigEntry = {
  values: SshConfigFields
  start: number
  end: number
  options: Partial<Record<Field, { index: number; prefix: string; comment: string }>>
}

export const blankSshHost: SshConfigFields = { alias: "", hostname: "", user: "", port: "", identityFile: "" }

function linesOf(config: string) {
  return config.match(/[^\n]*\n|[^\n]+$/g) ?? []
}

function directive(line: string) {
  const m = /^([ \t]*)([a-z]+)([ \t]*=[ \t]*|[ \t]+)([^\r\n]*)/i.exec(line)
  return m ? { name: m[2].toLowerCase(), prefix: m[1] + m[2] + m[3], args: m[4] } : null
}

function words(args: string) {
  const matches = args.match(/"[^"]*"|'[^']*'|[^ \t]+/g) ?? []
  const names: string[] = []
  for (const word of matches) {
    if (word.startsWith("#")) break
    names.push(word.replace(/^(["'])(.*)\1$/, "$2"))
  }
  return names
}

function readOption(args: string) {
  const match = /^("[^"]*"|'[^']*'|[^\s#"']+)([ \t]*(?:#.*)?)$/.exec(args)
  if (!match) return null
  return { value: match[1].replace(/^(["'])(.*)\1$/, "$2"), comment: match[2] }
}

/** Only unambiguous, single-alias Host blocks are eligible for the form. */
export function editableSshHosts(config: string): SshConfigEntry[] {
  const lines = linesOf(config)
  const boundaries = lines.flatMap((line, index) => {
    const d = directive(line)
    return d && (d.name === "host" || d.name === "match") ? [{ index, names: d.name === "host" ? words(d.args) : [] }] : []
  })
  const result: SshConfigEntry[] = []
  for (const [i, boundary] of boundaries.entries()) {
    const { index: start, names } = boundary
    const alias = names[0]
    if (names.length !== 1 || !aliasPattern.test(alias)) continue
    if (boundaries.some((other) => other !== boundary && other.names.some((name) => name.toLowerCase() === alias.toLowerCase()))) continue
    let end = boundaries[i + 1]?.index ?? lines.length
    // Leave comments and spacing between stanzas in place when removing one.
    while (end > start + 1) {
      const last = lines[end - 1].trim()
      if (last && !last.startsWith("#")) break
      end--
    }
    const values = { ...blankSshHost, alias }
    const options: SshConfigEntry["options"] = {}
    let editable = true
    for (let line = start + 1; line < end; line++) {
      const d = directive(lines[line])
      if (!d) continue
      const field = fields.find((f) => directives[f].toLowerCase() === d.name)
      if (!field) continue
      const parsed = readOption(d.args)
      if (!parsed || options[field]) {
        editable = false
        break
      }
      values[field] = parsed.value
      options[field] = { index: line, prefix: d.prefix, comment: parsed.comment }
    }
    if (editable) result.push({ values, start, end, options })
  }
  return result
}

function cleaned(values: SshConfigFields): SshConfigFields {
  return {
    alias: values.alias.trim(),
    hostname: values.hostname.trim(),
    user: values.user.trim(),
    port: values.port.trim(),
    identityFile: values.identityFile.trim(),
  }
}

function validate(values: SshConfigFields, previous?: SshConfigFields) {
  if (!aliasPattern.test(values.alias)) throw new Error("Use letters, numbers, dots, dashes, or underscores for the alias.")
  if (values.hostname !== previous?.hostname && !/^[a-z0-9_.:%-]+$/i.test(values.hostname)) {
    throw new Error("Enter a hostname or IP address.")
  }
  if (values.user !== previous?.user && /[\s#"'\x00-\x1f\x7f]/.test(values.user)) throw new Error("Enter an SSH user without spaces or quotes.")
  if (values.port !== previous?.port && values.port && (!/^\d+$/.test(values.port) || Number(values.port) < 1 || Number(values.port) > 65535)) {
    throw new Error("Enter a port from 1 to 65535.")
  }
  if (values.identityFile !== previous?.identityFile && /["'\x00-\x1f\x7f]/.test(values.identityFile)) {
    throw new Error("Enter the identity file path without surrounding quotes.")
  }
}

function encode(value: string) {
  return /[\s#]/.test(value) ? `"${value}"` : value
}

/** Replace only changed options, preserving other lines and directives. */
export function writeSshHost(config: string, input: SshConfigFields, existing?: SshConfigEntry): string {
  const values = cleaned(input)
  validate(values, existing?.values)
  const eol = config.includes("\r\n") ? "\r\n" : "\n"
  const lines = linesOf(config)
  if (!existing) {
    const first = lines.findIndex((line) => {
      const name = directive(line)?.name
      return name === "host" || name === "match"
    })
    const at = first === -1 ? lines.length : first
    const before = lines.slice(0, at).join("")
    const after = lines.slice(at).join("")
    const block = [`Host ${values.alias}`, ...fields.filter((f) => values[f]).map((f) => `    ${directives[f]} ${encode(values[f])}`)]
    return before + (before ? (before.endsWith("\n") ? eol : eol + eol) : "") + block.join(eol) + eol + (after ? eol + after : "")
  }
  if (values.alias !== existing.values.alias) throw new Error("Use the raw config editor to rename an existing alias.")
  const added: string[] = []
  for (const field of fields) {
    const value = values[field]
    if (value === existing.values[field]) continue
    const prior = existing.options[field]
    if (!prior) {
      if (value) added.push(`    ${directives[field]} ${encode(value)}${eol}`)
      continue
    }
    const lineEnding = /\r?\n$/.exec(lines[prior.index])?.[0] ?? ""
    lines[prior.index] = value
      ? `${prior.prefix}${encode(value)}${prior.comment}${lineEnding}`
      : prior.comment.trim().startsWith("#") ? `${prior.comment.trimStart()}${lineEnding}` : ""
  }
  if (added.length) {
    const before = lines.slice(0, existing.end).join("")
    return before + (before.endsWith("\n") ? "" : eol) + added.join("") + lines.slice(existing.end).join("")
  }
  return lines.join("")
}

export function removeSshHost(config: string, entry: SshConfigEntry): string {
  const lines = linesOf(config)
  lines.splice(entry.start, entry.end - entry.start)
  return lines.join("")
}
