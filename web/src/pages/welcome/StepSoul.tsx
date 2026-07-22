/**
 * Step 6 — Soul. Collects the companion's name and a free-text seed.
 * Saves nothing: the seed is carried to the Done step, which deep-links
 * into the Soul Studio (/soul?seed=…&from=welcome).
 */
import { useState } from "react";
import { C, Field, StepFooter, StepTitle, TextArea, TextInput } from "./ui";

export default function StepSoul({
  initialName,
  initialSeed,
  onBack,
  onDone,
}: {
  initialName: string;
  initialSeed: string;
  onBack: () => void;
  onDone: (name: string, seed: string) => void;
}) {
  const [name, setName] = useState(initialName);
  const [seed, setSeed] = useState(initialSeed);

  const ready = name.trim() !== "";

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        if (ready) onDone(name.trim(), seed.trim());
      }}
    >
      <StepTitle
        kicker="Step 6 — Soul"
        title="Who are they?"
        sub="Nothing is written yet — these words seed the Soul Studio, where their personality takes shape."
      />

      <Field label="Their name" hint="What you'll call them, face to face.">
        <TextInput
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Ada, Juno, Atlas…"
          autoFocus
        />
      </Field>

      <Field
        label="Who are they to you?"
        hint="Free-form. A confidant? A co-founder? A ship's computer with opinions? Everything you write here becomes raw material for their soul."
      >
        <TextArea
          value={seed}
          onChange={(e) => setSeed(e.target.value)}
          rows={6}
          placeholder="They're the friend who remembers everything I forget — dry humor, endlessly patient, a little protective…"
        />
      </Field>

      <p style={{ color: C.faint, fontSize: 12.5, lineHeight: 1.6 }}>
        At the end of setup you can open the Soul Studio pre-seeded with this
        text, or head straight to your first face-to-face conversation.
      </p>

      <StepFooter onBack={onBack} continueDisabled={!ready} />
    </form>
  );
}
