import type { DiffView, Entry } from "./App";
import * as I from "./Icons";

type ToolEntry = Extract<Entry, { kind: "tool" }>;

/**
 * A change, the way you review it before it is made.
 *
 * Rendered from the same plan Rust writes — odin_core::edit — so this is not a
 * preview of the edit, it *is* the edit. Line numbers on both sides, three lines
 * of context, and a gap marker between separate changes.
 */
export function Diff({ diff }: { diff: DiffView }) {
  return (
    <div className="diff" role="table" aria-label="Changes">
      {diff.hunks.map((h, i) => (
        <div key={i} className="hunk">
          {i > 0 && <div className="hunk-gap" aria-hidden="true">⋯</div>}
          {h.lines.map((l, j) => (
            <div
              key={j}
              role="row"
              className={"dl " + (l.tag === "+" ? "add" : l.tag === "-" ? "del" : "same")}
            >
              <span className="ln">{l.old ?? ""}</span>
              <span className="ln">{l.new ?? ""}</span>
              <span className="sign">{l.tag === " " ? "" : l.tag === "-" ? "−" : "+"}</span>
              <span className="dl-text">{l.text || " "}</span>
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}

/** Why a request went straight back to the model, in words — the full message
 *  is one click away. */
function reasonOf(error = ""): string {
  // The read-range refusals by their own wording: a missed edit in a big file
  // also mentions start_line, in its advice, and must not land here.
  if (/(start|end)_line (must be a line number|is the last line to read)/.test(error) || error.includes("is past the end"))
    return "the lines it asked for make no sense";
  if (error.includes("was not found")) return "the text it gave isn't in the file";
  if (/appears \d+ times/.test(error)) return "that text appears in more than one place";
  if (error.includes("identical")) return "the change would not have changed anything";
  if (error.includes("outside the project folder")) return "that file is outside the project folder";
  if (error.includes("inside .git")) return "files inside .git are never edited";
  if (error.includes("file stream")) return "that names a file stream";
  if (error.includes("editing is off")) return "no project folder is chosen";
  if (error.includes("already exists")) return "that file already exists";
  if (error.includes("is an existing folder")) return "that is a folder, not a file name";
  if (error.includes("is a link") || error.includes("reached through a link")) return "that file is reached through a link";
  if (error.includes("is read-only")) return "that file is read-only";
  if (error.includes("byte limit for a delete")) return "that file is too big to delete";
  if (error.includes("same file")) return "that is the same file, only the case differs";
  if (error.includes("different drives")) return "that is on another drive";
  if (error.includes("Windows")) return "Windows cannot use that name";
  if (error.includes("new folders; at most")) return "that path would make too many folders";
  if (error.includes("a file, not a folder")) return "a file is in the way of that path";
  if (error.includes("without a destination")) return "it gave no place to move the file to";
  if (error.includes("updated the plan")) return "it updated its task list too many times this turn";
  if (error.includes("without any steps")) return "its task list had no steps";
  if (error.includes("could not be read") || error.includes("valid JSON")) return "the request was malformed";
  if (error.includes("does not exist")) return "that file doesn't exist";
  if (error.includes("is a folder")) return "that is a folder, not a file";
  if (error.includes("decides what the check command runs")) return "that file decides what the check runs";
  if (error.includes("own settings folder")) return "that is Mímir's own settings folder";
  if (error.includes("no check command is set")) return "no check command is chosen for this folder";
  if (error.includes("no project folder to search")) return "no project folder is chosen";
  if (error.includes("is not inside the project folder")) return "that pattern reaches outside the project folder";
  if (error.includes("one line at a time")) return "it searched for more than one line";
  if (error.includes("without text")) return "it gave nothing to search for";
  return "it could not be carried out";
}

const VERB: Record<string, string> = {
  edit_file: "Couldn't edit",
  create_file: "Couldn't create",
  delete_file: "Couldn't delete",
  move_file: "Couldn't move",
  read_file: "Couldn't read",
  run_check: "Couldn't run the check",
  search: "Couldn't search",
  list_files: "Couldn't list files",
  update_plan: "Couldn't update the task list",
};

/** Marks a call that ran without asking: said once, quietly, where the state is. */
function AutoTag() {
  return (
    <span
      className="tool-auto"
      title="In the project folder. Set ask_every_call in config.json to be asked every time."
    >
      no approval needed
    </span>
  );
}

/**
 * A search or a listing the model wants: what it is looking for, and where.
 * Reads nothing outside the project folder and writes nothing, so inside a
 * folder you chose it runs without asking (H5) and arrives as a ran card marked
 * `auto`. It still asks when `ask_every_call` is set in the config, and the
 * pending card below is that case.
 */
function FindCard({ entry, onAllow, onDeny }: { entry: ToolEntry; onAllow: () => void; onDeny: () => void }) {
  const { name, path, state } = entry;
  const failed = entry.ok === false;
  // The result's first line says what was found — "7 matches in 3 files".
  const found = entry.result?.split("\n")[0].replace(/[:.]$/, "") ?? "";
  const label =
    state === "pending"
      ? "Needs approval"
      : state === "running"
        ? "Looking"
        : state === "stopped"
          ? "Stopped"
          : state === "denied"
            ? "Denied"
            : failed
              ? "Failed"
              : found;

  return (
    <div className={"tool find " + state + (failed ? " failed" : "") + (entry.auto ? " auto" : "")}>
      <div className="tool-head">
        <span className="tool-icon">
          <I.Search />
        </span>
        <span className="tool-name">{name === "search" ? "Search" : "List files"}</span>
        <code className="tool-path" title={path}>
          <bdi dir="ltr">{path}</bdi>
        </code>
        {entry.auto && <AutoTag />}
        <span className="tool-state">{label}</span>
      </div>
      {state === "pending" && (
        <div className="tool-ask">
          <p>
            The model wants to look through the project folder for this. Nothing has been read yet — it
            looks only if you allow it.
          </p>
          <div className="tool-buttons">
            <button className="btn primary sm" onClick={onAllow}>
              Allow once
            </button>
            <button className="btn ghost sm" onClick={onDeny}>
              Deny
            </button>
          </div>
        </div>
      )}
      {state === "denied" && <p className="tool-note">You declined. Nothing was read, and the model was told.</p>}
      {(state === "ran" || state === "stopped") && entry.result && (
        <details className="tool-result">
          <summary>{failed ? "Why it failed" : "Show what was found"}</summary>
          <pre>{entry.result}</pre>
        </details>
      )}
    </div>
  );
}

/**
 * A check the model wants to run: the folder's own command, never one the model
 * wrote. The card names it, says when build files changed first, and shows what
 * came back. Red for a failed build is the result, not a fault in the tool.
 */
function CheckCard({ entry, onAllow, onDeny }: { entry: ToolEntry; onAllow: () => void; onDeny: () => void }) {
  const { path, state } = entry;
  const passed = entry.ok !== false;
  const label =
    state === "pending"
      ? "Needs approval"
      : state === "running"
        ? "Running"
        : state === "stopped"
          ? "Stopped"
          : state === "denied"
            ? "Denied"
            : passed
              ? "Passed"
              : "Failed";
  const changed = entry.changedBuildFiles ?? [];

  return (
    <div className={"tool check " + state + (state === "ran" && !passed ? " fail" : "")}>
      <div className="tool-head">
        <span className="tool-icon">
          <I.Terminal />
        </span>
        <span className="tool-name">Run check</span>
        <code className="tool-path" title={path}>
          <bdi dir="ltr">{path}</bdi>
        </code>
        <span className="tool-state">{label}</span>
      </div>

      {state === "pending" && (
        <>
          {changed.length > 0 && (
            <p className="tool-warn">
              Changed earlier in this answer, and the check will run{" "}
              {changed.length === 1 ? "it" : "them"}:{" "}
              {changed.map((f, i) => (
                <span key={f}>
                  {i > 0 && ", "}
                  <code>{f}</code>
                </span>
              ))}
              .
            </p>
          )}
          <div className="tool-ask">
            <p>
              The model wants to run this in the project folder. Nothing has run yet — it runs only if
              you allow it.
            </p>
            <div className="tool-buttons">
              <button className="btn primary sm" onClick={onAllow}>
                Run once
              </button>
              <button className="btn ghost sm" onClick={onDeny}>
                Deny
              </button>
            </div>
          </div>
        </>
      )}
      {state === "running" && <p className="tool-note">Running in the project folder. Stop ends it.</p>}
      {state === "denied" && <p className="tool-note">You declined. Nothing was run, and the model was told.</p>}
      {(state === "ran" || state === "stopped") && entry.result && (
        <details className="tool-result">
          <summary>Show output</summary>
          <pre>{entry.result}</pre>
        </details>
      )}
    </div>
  );
}

/**
 * The model's task list for a multi-step request (H10), as a checklist.
 *
 * It touches nothing, so it never asks and has no buttons: it is there so you can
 * see what the model means to do and how far it has got. Each update replaces the
 * last one — the thread draws only the latest card of a turn — so this is one card
 * that fills in, not a card per update. The steps are what Rust parsed from the
 * model's own lines, never its raw text.
 */
function PlanCard({ entry }: { entry: ToolEntry }) {
  const steps = entry.plan ?? [];
  const done = steps.filter((p) => p.status === "done").length;
  return (
    <div className={"tool plan " + entry.state}>
      <div className="tool-head">
        <span className="tool-icon">
          <I.Checklist />
        </span>
        <span className="tool-name plan-title">
          Plan · {done} of {steps.length} done
        </span>
      </div>
      <ul className="plan-steps">
        {steps.map((p, i) => (
          <li
            key={i}
            className={"plan-step " + p.status}
            aria-label={p.status === "pending" ? p.text : `${p.text} (${p.status === "done" ? "done" : "in progress"})`}
          >
            <span className="plan-mark" aria-hidden="true" />
            <span className="plan-text">{p.text}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** A size a person reads. */
function sizeOf(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** The folders a create or a move makes, before and after. */
function FolderNote({ folders, made }: { folders: string[]; made?: boolean }) {
  return (
    <p className="tool-note tool-folders">
      {made ? "Made the folder" : "Makes the folder"}
      {folders.length === 1 ? "" : "s"}:{" "}
      {folders.map((d, i) => (
        <span key={d}>
          {i > 0 && ", "}
          <code>{d}</code>
        </span>
      ))}
      {made ? "." : " — taken away again if you undo."}
    </p>
  );
}

/**
 * A tool this build does not draw a card for. It says what it is and shows what
 * came back, and never presents itself as a read of a file.
 */
function PlainCard({ entry, onAllow, onDeny }: { entry: ToolEntry; onAllow: () => void; onDeny: () => void }) {
  const { name, path, state } = entry;
  return (
    <div className={"tool plain " + state}>
      <div className="tool-head">
        <span className="tool-icon">
          <I.File />
        </span>
        <span className="tool-name">{name}</span>
        {path && (
          <code className="tool-path" title={path}>
            <bdi dir="ltr">{path}</bdi>
          </code>
        )}
        <span className="tool-state">{state === "pending" ? "Needs approval" : state}</span>
      </div>
      {state === "pending" && (
        <div className="tool-ask">
          <div className="tool-buttons">
            <button className="btn primary sm" onClick={onAllow}>
              Allow once
            </button>
            <button className="btn ghost sm" onClick={onDeny}>
              Deny
            </button>
          </div>
        </div>
      )}
      {state === "ran" && entry.result && (
        <details className="tool-result">
          <summary>Show result</summary>
          <pre>{entry.result.slice(0, 4000)}</pre>
        </details>
      )}
    </div>
  );
}

export function ToolCard({
  entry,
  onAllow,
  onDeny,
  onUndo,
  disabled,
}: {
  entry: ToolEntry;
  onAllow: () => void;
  onDeny: () => void;
  onUndo: () => void;
  /** Undo waits: an answer is streaming, or another undo is running. */
  disabled: boolean;
}) {
  const { name, path, state } = entry;

  // Never shown to you for a decision: it could not have worked, so it went
  // back to the model with the reason. Quiet, but visible — nothing happens
  // unseen.
  if (state === "rejected") {
    return (
      <details className="tool-rejected">
        <summary>
          <I.X width={13} height={13} />
          <span>
            {VERB[name] ?? "Couldn't run"} {path ? <code>{path}</code> : null} — {reasonOf(entry.error)}.
            The model was told.
          </span>
        </summary>
        <pre>{entry.error}</pre>
      </details>
    );
  }

  if (name === "update_plan") return <PlanCard entry={entry} />;
  if (name === "run_check") return <CheckCard entry={entry} onAllow={onAllow} onDeny={onDeny} />;
  if (name === "search" || name === "list_files") return <FindCard entry={entry} onAllow={onAllow} onDeny={onDeny} />;

  // By name, not by elimination: until H9 anything that was not an edit fell
  // through to the read card, so a tool added later would have been shown as one.
  const change = name === "edit_file" || name === "create_file" || name === "delete_file" || name === "move_file";

  if (!change && name !== "read_file") return <PlainCard entry={entry} onAllow={onAllow} onDeny={onDeny} />;

  if (!change) {
    // `ok` from Rust; a thread saved before it existed says so only in the text.
    const failed = entry.ok === false || entry.result?.startsWith("error:");
    return (
      <div className={"tool read " + state + (failed ? " failed" : "") + (entry.auto ? " auto" : "")}>
        <div className="tool-head">
          <span className="tool-icon">
            <I.File />
          </span>
          <span className="tool-name">Read file</span>
          <code className="tool-path" title={path}>
            <bdi dir="ltr">{path}</bdi>
          </code>
          {entry.range && (
            <span className="tool-range">
              {entry.range.end != null ? `lines ${entry.range.start}–${entry.range.end}` : `from line ${entry.range.start}`}
            </span>
          )}
          {entry.auto && <AutoTag />}
          <span className="tool-state">
            {state === "pending"
              ? "Needs approval"
              : state === "running"
                ? "Reading"
                : state === "stopped"
                  ? "Stopped"
                  : state === "denied"
                    ? "Denied"
                    : failed
                      ? "Failed"
                      : `${(entry.result?.length ?? 0).toLocaleString()} chars`}
          </span>
        </div>

        {state === "pending" && (
          <div className="tool-ask">
            <p>
              The model wants to read this file. Nothing has been read yet — its contents go to the
              model only if you allow it.
            </p>
            <div className="tool-buttons">
              <button className="btn primary sm" onClick={onAllow}>
                Allow once
              </button>
              <button className="btn ghost sm" onClick={onDeny}>
                Deny
              </button>
            </div>
          </div>
        )}
        {state === "denied" && <p className="tool-note">You declined. The model was told.</p>}
        {state === "ran" && (
          <details className="tool-result">
            <summary>{failed ? "Why it failed" : "Show what was read"}</summary>
            <pre>{entry.result?.slice(0, 4000)}</pre>
          </details>
        )}
      </div>
    );
  }

  const create = name === "create_file" || entry.diff?.create;
  const del = name === "delete_file";
  const move = name === "move_file";
  const folders = entry.folders ?? [];
  const label =
    state === "pending"
      ? "Review"
      : state === "running"
        ? "Applying"
        : state === "ran"
          ? del
            ? "Deleted"
            : move
              ? "Moved"
              : "Applied"
          : state === "failed"
            ? "Not written"
            : state === "undone"
              ? "Undone"
              : state === "stopped"
                ? "Stopped"
                : "Declined";

  return (
    <div className={"tool edit " + (del ? "delete " : move ? "move " : create ? "create " : "") + state}>
      <div className="tool-head">
        <span className="tool-icon">
          {del ? <I.Trash /> : move ? <I.Move /> : create ? <I.Plus /> : <I.Pencil />}
        </span>
        <span className="tool-name">
          {del ? "Delete file" : move ? "Move file" : create ? "Create file" : "Edit file"}
        </span>
        <code className="tool-path" title={move && entry.to ? `${path} → ${entry.to}` : path}>
          <bdi dir="ltr">{move && entry.to ? `${path} → ${entry.to}` : path}</bdi>
        </code>
        {(del || move) && entry.size != null && <span className="tool-range">{sizeOf(entry.size)}</span>}
        {entry.diff && (
          <span className="counts" aria-label={`${entry.diff.added} lines added, ${entry.diff.removed} removed`}>
            <span className="plus">+{entry.diff.added}</span>
            <span className="minus">−{entry.diff.removed}</span>
          </span>
        )}
        <span className="tool-state">{label}</span>
        {state === "ran" && entry.undo && (
          <button
            className="btn ghost xs"
            onClick={onUndo}
            disabled={disabled}
            title={
              del
                ? "Put the file back"
                : move
                  ? "Move the file back"
                  : create
                    ? "Remove the file again"
                    : "Put the file back the way it was"
            }
          >
            Undo
          </button>
        )}
      </div>

      {state === "pending" && (
        <>
          {folders.length > 0 && <FolderNote folders={folders} />}
          {entry.diff && <Diff diff={entry.diff} />}
          <div className="tool-ask">
            <p>
              {del
                ? "Nothing has been deleted yet. Deleting keeps a copy of the file, so it can be undone."
                : move
                  ? "Nothing has been moved yet. The file keeps its contents, nothing is overwritten, and moving it back is one click. References to it are not updated."
                  : "Nothing has been changed yet. Applying writes exactly this — and keeps a backup, so it can be undone."}
            </p>
            <div className="tool-buttons">
              <button className="btn primary sm" onClick={onAllow}>
                {del ? "Delete file" : move ? "Move file" : create ? "Create file" : "Apply edit"}
              </button>
              <button className="btn ghost sm" onClick={onDeny}>
                Deny
              </button>
            </div>
          </div>
        </>
      )}

      {(state === "ran" || state === "undone") && entry.diff && (
        <details className="tool-result">
          <summary>{state === "undone" ? "Show what was undone" : "Show changes"}</summary>
          <Diff diff={entry.diff} />
        </details>
      )}
      {state === "ran" && folders.length > 0 && <FolderNote folders={folders} made />}
      {state === "undone" && (
        <p className="tool-note">
          {del
            ? "Undone — the file is back."
            : move
              ? "Undone — the file is moved back."
              : create
                ? "Undone — the file is removed."
                : "Undone — the file is back as it was."}
        </p>
      )}
      {state === "denied" && (
        <p className="tool-note">
          {del
            ? "You declined. Nothing was deleted, and the model was told."
            : move
              ? "You declined. Nothing was moved, and the model was told."
              : "You declined. Nothing was written, and the model was told."}
        </p>
      )}
      {state === "failed" && <p className="tool-note err">{entry.error?.replace(/^error:\s*/, "")}</p>}
    </div>
  );
}
