import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatEditorTime } from "./segmentEditor";
import type {
  CapabilityTarget,
  GlossaryTerm,
  MediaItem,
  ProposalDecision,
  RefineOutcome,
  RefineProposal,
  Segment,
} from "./types";

type Props = {
  media: MediaItem;
  segments: Segment[];
  onSegmentUpdated: (segment: Segment) => void;
  onSelectSegment: (segmentId: number) => void;
};

const targetKey = (target: CapabilityTarget) =>
  `${target.provider_id}\u0000${target.model_id}`;

function splitAliases(value: string) {
  return value
    .split(/[,，、\n]/)
    .map((alias) => alias.trim())
    .filter(Boolean);
}

export default function RefinePanel({
  media,
  segments,
  onSegmentUpdated,
  onSelectSegment,
}: Props) {
  const [targets, setTargets] = useState<CapabilityTarget[]>([]);
  const [selectedKey, setSelectedKey] = useState("");
  const [proposals, setProposals] = useState<RefineProposal[]>([]);
  const [glossary, setGlossary] = useState<GlossaryTerm[]>([]);
  const [glossaryOpen, setGlossaryOpen] = useState(false);
  const [newTerm, setNewTerm] = useState("");
  const [newAliases, setNewAliases] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");

  const usable = useMemo(
    () =>
      targets.filter(
        (target) => target.available && target.provider_availability === "ready",
      ),
    [targets],
  );
  const selected = useMemo(
    () => usable.find((target) => targetKey(target) === selectedKey) ?? null,
    [selectedKey, usable],
  );
  const startBySegment = useMemo(
    () => new Map(segments.map((segment) => [segment.id, segment.start_ms])),
    [segments],
  );

  const load = useCallback(async () => {
    const [rows, pending, terms] = await Promise.all([
      invoke<CapabilityTarget[]>("resolve_capability_targets", {
        capability: "transcript.refine",
      }),
      invoke<RefineProposal[]>("list_refine_proposals", { mediaId: media.id }),
      invoke<GlossaryTerm[]>("list_glossary"),
    ]);
    setTargets(rows);
    setProposals(pending);
    setGlossary(terms);
    setSelectedKey((current) => {
      const ready = rows.filter(
        (target) => target.available && target.provider_availability === "ready",
      );
      if (ready.some((target) => targetKey(target) === current)) return current;
      return ready[0] ? targetKey(ready[0]) : "";
    });
  }, [media.id]);

  useEffect(() => {
    setMessage("");
    void load().catch((error) => setMessage(String(error)));
  }, [load]);

  const check = async () => {
    if (!selected) return;
    setBusy(true);
    setMessage("Checking the transcript…");
    try {
      const outcome = await invoke<RefineOutcome>("refine_transcript", {
        input: {
          media_id: media.id,
          provider_id: selected.provider_id,
          model_id: selected.model_id,
          segment_ids: null,
          language: null,
        },
      });
      setProposals(outcome.proposals);
      const skipped = outcome.notes.length
        ? ` ${outcome.notes.length} item(s) were skipped as unsafe or invalid.`
        : "";
      setMessage(
        outcome.new_proposals === 0
          ? `Checked ${outcome.checked_segments} lines. Nothing to fix.${skipped}`
          : `Checked ${outcome.checked_segments} lines. ${outcome.new_proposals} suggestion(s) to review.${skipped}`,
      );
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  };

  const handleDecision = (proposal: RefineProposal, decision: ProposalDecision) => {
    setProposals((rows) => rows.filter((row) => row.id !== proposal.id));
    if (decision.outcome === "applied") {
      onSegmentUpdated(decision.segment);
    } else if (decision.outcome === "stale") {
      if (decision.segment) onSegmentUpdated(decision.segment);
      setMessage("That line was edited after the check, so its suggestion was discarded.");
    }
  };

  const accept = async (proposal: RefineProposal) => {
    try {
      const decision = await invoke<ProposalDecision>("accept_refine_proposal", {
        id: proposal.id,
      });
      handleDecision(proposal, decision);
    } catch (error) {
      setMessage(String(error));
    }
  };

  const skip = async (proposal: RefineProposal) => {
    try {
      const decision = await invoke<ProposalDecision>("reject_refine_proposal", {
        id: proposal.id,
      });
      handleDecision(proposal, decision);
    } catch (error) {
      setMessage(String(error));
    }
  };

  const acceptAll = async () => {
    setBusy(true);
    let stale = 0;
    for (const proposal of proposals) {
      try {
        const decision = await invoke<ProposalDecision>("accept_refine_proposal", {
          id: proposal.id,
        });
        if (decision.outcome === "stale") stale += 1;
        handleDecision(proposal, decision);
      } catch (error) {
        setMessage(String(error));
        break;
      }
    }
    setBusy(false);
    if (stale) setMessage(`${stale} suggestion(s) were discarded because those lines changed.`);
  };

  const addTerm = async () => {
    if (!newTerm.trim()) return;
    try {
      const saved = await invoke<GlossaryTerm>("save_glossary_term", {
        input: { id: null, term: newTerm, aliases: splitAliases(newAliases), note: "" },
      });
      setGlossary((rows) =>
        [...rows, saved].sort((a, b) => a.term.localeCompare(b.term)),
      );
      setNewTerm("");
      setNewAliases("");
    } catch (error) {
      setMessage(String(error));
    }
  };

  const removeTerm = async (term: GlossaryTerm) => {
    try {
      await invoke("delete_glossary_term", { id: term.id });
      setGlossary((rows) => rows.filter((row) => row.id !== term.id));
    } catch (error) {
      setMessage(String(error));
    }
  };

  return (
    <section className="refine-panel">
      <div className="refine-head">
        <div>
          <p className="section-label">REVIEW</p>
          <strong>Fix recognition mistakes</strong>
          <small>Suggestions never change your transcript until you accept them.</small>
        </div>
      </div>

      {targets.length === 0 ? (
        <div className="refine-empty">
          Add a text model that can refine transcripts in Settings → Providers. A local
          server such as LM Studio works the same as a paid API.
        </div>
      ) : (
        <div className="refine-run">
          <select
            value={selectedKey}
            onChange={(event) => setSelectedKey(event.target.value)}
            aria-label="Refinement model"
          >
            {!usable.length && <option value="">No model is ready</option>}
            {usable.map((target) => (
              <option key={targetKey(target)} value={targetKey(target)}>
                {target.provider_name} · {target.display_name || target.model_id}
              </option>
            ))}
          </select>
          <button
            className="primary"
            disabled={busy || !selected || segments.length === 0}
            onClick={() => void check()}
          >
            {busy ? "Checking…" : "Check transcript"}
          </button>
        </div>
      )}

      <div className="glossary">
        <button
          className="glossary-toggle"
          aria-expanded={glossaryOpen}
          onClick={() => setGlossaryOpen((open) => !open)}
        >
          Names &amp; terms <span>{glossary.length}</span>
        </button>
        {glossaryOpen && (
          <div className="glossary-body">
            <p className="glossary-help">
              Add the correct spelling and how it might be misheard or romanized. Every
              check uses this list.
            </p>
            {glossary.map((term) => (
              <div className="glossary-term" key={term.id}>
                <strong>{term.term}</strong>
                <small>{term.aliases.join(" · ") || "no aliases"}</small>
                <button
                  className="icon"
                  aria-label={`Remove ${term.term}`}
                  onClick={() => void removeTerm(term)}
                >
                  ×
                </button>
              </div>
            ))}
            <div className="glossary-add">
              <input
                value={newTerm}
                onChange={(event) => setNewTerm(event.target.value)}
                placeholder="Correct spelling, e.g. 秋山はるる"
              />
              <input
                value={newAliases}
                onChange={(event) => setNewAliases(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") void addTerm();
                }}
                placeholder="Also written as, e.g. Haruru Akiyama, あきやまはるる"
              />
              <button className="secondary" disabled={!newTerm.trim()} onClick={() => void addTerm()}>
                Add
              </button>
            </div>
          </div>
        )}
      </div>

      {message && <div className="refine-message">{message}</div>}

      {proposals.length > 0 && (
        <div className="proposal-list">
          <div className="proposal-list-head">
            <strong>{proposals.length} to review</strong>
            <button className="secondary" disabled={busy} onClick={() => void acceptAll()}>
              Accept all
            </button>
          </div>
          {proposals.map((proposal) => (
            <article
              className="proposal"
              key={proposal.id}
              onClick={() => onSelectSegment(proposal.segment_id)}
            >
              <time>
                {formatEditorTime(startBySegment.get(proposal.segment_id) ?? 0)}
              </time>
              <p className="proposal-diff">
                {proposal.diff.map((op, index) =>
                  op.kind === "equal" ? (
                    <span key={index}>{op.text}</span>
                  ) : op.kind === "delete" ? (
                    <del key={index}>{op.text}</del>
                  ) : (
                    <ins key={index}>{op.text}</ins>
                  ),
                )}
              </p>
              <footer>
                <small>
                  {proposal.reason || "Recognition fix"}
                  {proposal.confidence !== null &&
                    ` · ${Math.round(proposal.confidence * 100)}% sure`}
                </small>
                <span className="proposal-actions">
                  <button
                    className="secondary"
                    onClick={(event) => {
                      event.stopPropagation();
                      void skip(proposal);
                    }}
                  >
                    Skip
                  </button>
                  <button
                    className="primary"
                    onClick={(event) => {
                      event.stopPropagation();
                      void accept(proposal);
                    }}
                  >
                    Accept
                  </button>
                </span>
              </footer>
            </article>
          ))}
        </div>
      )}
    </section>
  );
}
