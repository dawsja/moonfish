import { describe, expect, it } from "vitest"
import { blankSshHost, editableSshHosts, removeSshHost, writeSshHost } from "./ssh-config"

describe("SSH host forms", () => {
  it("adds a specific host before wildcard and Match rules", () => {
    const config = "Include conf.d/*\nHost *\n    Port 22\nMatch all\n    ForwardAgent no\n"
    const next = writeSshHost(config, { ...blankSshHost, alias: "build", hostname: "2001:db8::1", port: "2222", identityFile: "C:/SSH Keys/build" })
    expect(next).toBe('Include conf.d/*\n\nHost build\n    HostName 2001:db8::1\n    Port 2222\n    IdentityFile "C:/SSH Keys/build"\n\nHost *\n    Port 22\nMatch all\n    ForwardAgent no\n')
    expect(editableSshHosts(next)[0].values.identityFile).toBe("C:/SSH Keys/build")
  })

  it("updates only changed directives and keeps CRLF, comments and custom directives", () => {
    const config = '# Header\r\nHost build\r\n\tHostName=build.test # primary\r\n    ProxyJump jump\r\n    User alex\r\n\r\n# Next host\r\nHost other\r\n'
    const [build] = editableSshHosts(config)
    expect(writeSshHost(config, { ...build.values, hostname: "new.test", user: "", port: "2222" }, build)).toBe(
      '# Header\r\nHost build\r\n\tHostName=new.test # primary\r\n    ProxyJump jump\r\n    Port 2222\r\n\r\n# Next host\r\nHost other\r\n',
    )
    expect(removeSshHost(config, build)).toBe('# Header\r\n\r\n# Next host\r\nHost other\r\n')
  })

  it("keeps shared, repeated and multi-value entries in the raw config editor", () => {
    const config = 'Host shared other\nHost shared\nHost *.test\nHost one\n    IdentityFile a\n    IdentityFile b\nHost "ok"\nMatch all\n'
    expect(editableSshHosts(config).map((entry) => entry.values.alias)).toEqual(["ok"])
  })

  it("rejects invalid aliases, ports and multiline option values", () => {
    const fields = { ...blankSshHost, alias: "build", hostname: "build.test" }
    expect(() => writeSshHost("", { ...fields, alias: "*" })).toThrow("alias")
    expect(() => writeSshHost("", { ...fields, port: "65536" })).toThrow("port")
    expect(() => writeSshHost("", { ...fields, user: "alex\n    ProxyJump other" })).toThrow("SSH user")
  })
})
