import { beforeEach, describe, expect, it, vi } from "vitest"
import type { Status } from "@/lib/git"
import type { Note } from "@/lib/notes"
import { useNotes } from "./notes"

const status = vi.fn<() => Promise<Status>>()
vi.mock("@/lib/git", () => ({ git: { status: () => status() }, worktrees: {} }))
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn() }) }))

const A = "p1|"
const B = "p1|/work/tree"
const line = (n: number) => ({ cls: "a" as const, text: `line ${n}`, new: n })
const draft = (path: string, n: number): Omit<Note, "id"> => ({ path, from: { kind: "diff", staged: false }, lines: [line(n)], comment: "" })
const notes = (k: string) => useNotes.getState().byScope[k] ?? []

describe("line notes", () => {
  beforeEach(() => useNotes.setState({ byScope: {} }))

  it("starts empty, is written to, and removed", () => {
    const s = useNotes.getState()
    const id = s.add(A, draft("a.ts", 1))
    expect(notes(A)).toMatchObject([{ id, path: "a.ts", comment: "" }])
    s.write(A, id, "Why?")
    expect(notes(A)[0].comment).toBe("Why?")
    s.remove(A, id)
    // No list left behind for a workspace without notes.
    expect(useNotes.getState().byScope).toEqual({})
  })

  it("keeps each workspace's notes apart", () => {
    const s = useNotes.getState()
    s.write(A, s.add(A, draft("a.ts", 1)), "in the project")
    s.write(B, s.add(B, draft("a.ts", 1)), "in the worktree")
    expect(notes(A).map((n) => n.comment)).toEqual(["in the project"])
    expect(notes(B).map((n) => n.comment)).toEqual(["in the worktree"])
    expect(s.take(A)).toHaveLength(1)
    expect(notes(B)).toHaveLength(1)
  })

  it("gives up its written notes once, and drops the empty ones with them", () => {
    const s = useNotes.getState()
    s.write(A, s.add(A, draft("a.ts", 1)), "one")
    s.add(A, draft("a.ts", 2))
    s.write(A, s.add(A, draft("b.ts", 3)), "two")
    expect(s.take(A).map((n) => n.comment)).toEqual(["one", "two"])
    expect(notes(A)).toEqual([])
    expect(s.take(A)).toEqual([])
  })

  it("clears and says how many were written", () => {
    const s = useNotes.getState()
    s.write(A, s.add(A, draft("a.ts", 1)), "one")
    s.add(A, draft("a.ts", 2))
    expect(s.clear(A)).toBe(1)
    expect(notes(A)).toEqual([])
    expect(s.clear(A)).toBe(0)
  })

  it("ignores ids it doesn't have", () => {
    const s = useNotes.getState()
    s.write(A, s.add(A, draft("a.ts", 1)), "one")
    const before = useNotes.getState().byScope
    s.write(A, "nope", "x")
    s.remove(A, "nope")
    expect(useNotes.getState().byScope).toBe(before)
  })
})

describe("line notes and the branch", () => {
  const st = (branch: string | null, isRepo = true): Status => ({ isRepo, branch, head: "abc", upstream: null, ahead: 0, behind: 0, staged: [], unstaged: [], untracked: [], conflicted: [] })
  const scope = { projectId: "p1", cwd: null }

  beforeEach(() => useNotes.setState({ byScope: {} }))

  it("go when the workspace is on another branch, and only that workspace's", async () => {
    const { useGit } = await import("./git")
    useGit.setState({ status: {}, errors: {} })
    status.mockResolvedValue(st("main"))
    await useGit.getState().refresh(scope)
    const s = useNotes.getState()
    s.write(A, s.add(A, draft("a.ts", 1)), "keep on main")
    s.write(B, s.add(B, draft("a.ts", 1)), "another worktree")

    // Polls on the same branch keep them.
    await useGit.getState().refresh(scope)
    expect(notes(A)).toHaveLength(1)

    status.mockResolvedValue(st("feature"))
    await useGit.getState().refresh(scope)
    expect(notes(A)).toEqual([])
    expect(notes(B)).toHaveLength(1)

    // A detached HEAD is another place too.
    s.write(A, s.add(A, draft("a.ts", 1)), "on feature")
    status.mockResolvedValue(st(null))
    await useGit.getState().refresh(scope)
    expect(notes(A)).toEqual([])
  })

  it("stay when the first status comes in", async () => {
    const { useGit } = await import("./git")
    useGit.setState({ status: {}, errors: {} })
    const s = useNotes.getState()
    s.write(A, s.add(A, draft("a.ts", 1)), "before any status")
    status.mockResolvedValue(st("main"))
    await useGit.getState().refresh(scope)
    expect(notes(A)).toHaveLength(1)
  })
})
