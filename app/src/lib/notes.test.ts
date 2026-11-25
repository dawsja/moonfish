import { describe, expect, it } from "vitest"
import { parsePatch } from "@/components/chat/DiffView"
import { formatNotes, lineLabel, locate, noteRef, quote, span, takesNote, written, type Note, type ViewLine } from "./notes"

const PATCH = `diff --git a/src/app.ts b/src/app.ts
index 1111111..2222222 100644
--- a/src/app.ts
+++ b/src/app.ts
@@ -10,5 +10,6 @@ export function start() {
   const a = 1
-  const b = 2
+  const b = 3
+  const c = 4
   return a + b
 }
@@ -40,3 +41,3 @@ function stop() {
   log("bye")
-  exit(0)
+  exit(1)
 }`

const rows = parsePatch(PATCH)
const note = (lines: ViewLine[], comment: string, over: Partial<Note> = {}): Note => ({ id: "n", path: "src/app.ts", from: { kind: "diff", staged: false }, lines, comment, ...over })
const pick = (lo: number, hi: number) => rows.slice(lo, hi + 1)

describe("choosing lines", () => {
  it("reads the diff as the viewer shows it", () => {
    expect(rows.map((r) => r.cls).join("")).toBe("hcdaacchcdac")
    expect(rows[3]).toEqual({ cls: "a", text: "  const b = 3", new: 11 })
    expect(rows[2]).toEqual({ cls: "d", text: "  const b = 2", old: 11 })
  })

  it("takes lines either way round, and keeps to one hunk", () => {
    expect(span(rows, 2, 4)).toEqual([2, 4])
    expect(span(rows, 4, 2)).toEqual([2, 4])
    // Dragging past the next hunk's header stops at the end of this one.
    expect(span(rows, 4, 10)).toEqual([4, 6])
    expect(span(rows, 9, 1)).toEqual([8, 9])
    expect(span(rows, 3, 3)).toEqual([3, 3])
  })
})

describe("lines that take a note", () => {
  it("are real lines, not headers or what comes before the first hunk", () => {
    const fresh = parsePatch("diff --git a/x.ts b/x.ts\nnew file mode 100644\nindex 0000000..e69de29\n--- /dev/null\n+++ b/x.ts\n@@ -0,0 +1,2 @@\n+a\n+b")
    expect(fresh.map((l) => takesNote(l))).toEqual([false, false, true, true])
    expect(rows.filter((l) => takesNote(l))).toHaveLength(10)
    expect(takesNote(undefined)).toBe(false)
  })
})

describe("finding a note's lines again", () => {
  it("finds them where they are now", () => {
    expect(locate(rows, note(pick(2, 4), "x"))).toEqual([2, 4])
  })

  it("finds them after the diff around them moved", () => {
    const later = parsePatch(PATCH.replace("@@ -10,5 +10,6 @@ export function start() {\n", "@@ -10,5 +10,6 @@ export function start() {\n+// new\n").replace("+10,6", "+10,7"))
    // The added line pushed the new numbers down one: not the same lines any more.
    expect(locate(later, note(pick(3, 4), "x"))).toBeNull()
    // The removed line's old number didn't move.
    expect(locate(later, note([rows[2]], "x"))).toEqual([3, 3])
  })

  it("is lost once the code changed under it", () => {
    const edited = parsePatch(PATCH.replace("+  const b = 3", "+  const b = 30"))
    expect(locate(edited, note(pick(3, 4), "x"))).toBeNull()
  })

  it("follows an added line into the file as it is now", () => {
    const file: ViewLine[] = ["a", "b"].map((t, i) => ({ cls: "c", text: t, new: 11 + i }))
    expect(locate(file, note([{ cls: "a", text: "a", new: 11 }], "x"))).toEqual([0, 0])
    expect(locate(file, note([{ cls: "d", text: "a", old: 11 }], "x"))).toBeNull()
  })
})

describe("what a note says where it is", () => {
  it("numbers current lines as the file is now", () => {
    expect(noteRef(note(pick(3, 3), "x"))).toBe("`src/app.ts:11`")
    // A replaced line: the old line's number isn't the one to go to.
    expect(noteRef(note(pick(2, 4), "x"))).toBe("`src/app.ts:11-12`")
    expect(lineLabel(note(pick(2, 4), "x"))).toBe("Lines 11-12")
    expect(lineLabel(note(pick(3, 3), "x"))).toBe("Line 11")
  })

  it("numbers removed lines as they were", () => {
    expect(noteRef(note([rows[2]], "x"))).toBe("`src/app.ts:11` (removed)")
    expect(lineLabel(note([rows[2]], "x"))).toBe("Removed line 11")
  })

  it("names the commit for a commit's lines", () => {
    expect(noteRef(note(pick(3, 3), "x", { from: { kind: "commit", hash: "0123456789abcdef" } }))).toBe("`src/app.ts:11` in commit 01234567")
  })
})

describe("quoting", () => {
  it("quotes current code as code, in the file's language", () => {
    expect(quote(note(pick(3, 5), "x"))).toBe("```ts\n  const b = 3\n  const c = 4\n  return a + b\n```")
  })

  it("quotes a change that removes lines as a diff", () => {
    expect(quote(note(pick(1, 3), "x"))).toBe("```diff\n   const a = 1\n-  const b = 2\n+  const b = 3\n```")
  })

  it("quotes only removed lines as the old code", () => {
    expect(quote(note([rows[2]], "x"))).toBe("```ts\n  const b = 2\n```")
  })

  it("leaves the language off when the name has none", () => {
    expect(quote(note([{ cls: "c", text: "all:", new: 1 }], "x", { path: "Makefile" }))).toBe("```\nall:\n```")
  })

  it("fences code that has fences in it", () => {
    const md = note([{ cls: "c", text: "```js", new: 3 }], "x", { path: "README.md" })
    expect(quote(md)).toBe("````md\n```js\n````")
  })

  it("drops a carriage return left from a CRLF file", () => {
    expect(quote(note([{ cls: "c", text: "x = 1\r", new: 1 }], "x", { path: "a.py" }))).toBe("```py\nx = 1\n```")
  })
})

describe("the message for the chat", () => {
  it("is empty without written notes", () => {
    expect(formatNotes([])).toBe("")
    expect(formatNotes([note(pick(3, 3), "  \n")])).toBe("")
    expect(written([note(pick(3, 3), " "), note(pick(4, 4), "ok")]).map((n) => n.comment)).toEqual(["ok"])
  })

  it("puts each note's place, code and comment together, a file's notes top to bottom", () => {
    const notes: Note[] = [
      note(pick(10, 10), "Why exit with 1?", { id: "1" }),
      note([{ cls: "c", text: "# Moonfish", new: 1 }], "Say what it is in one line.", { id: "2", path: "README.md", from: { kind: "file" } }),
      note(pick(3, 4), "  Name these better.\nb and c say nothing.  ", { id: "3" }),
      note([rows[9]], "", { id: "4" }),
    ]
    expect(formatNotes(notes)).toBe(
      [
        "`src/app.ts:11-12`",
        "```ts",
        "  const b = 3",
        "  const c = 4",
        "```",
        "Name these better.",
        "b and c say nothing.",
        "",
        "`src/app.ts:42`",
        "```ts",
        "  exit(1)",
        "```",
        "Why exit with 1?",
        "",
        "`README.md:1`",
        "```md",
        "# Moonfish",
        "```",
        "Say what it is in one line.",
        "",
      ].join("\n"),
    )
  })
})
