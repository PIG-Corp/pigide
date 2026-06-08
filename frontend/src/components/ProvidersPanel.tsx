import { useCallback, useEffect, useState } from "react";
import { ipc } from "../state/ipc";
import { useStore } from "../state/store";
import type { ModelEntry, ProviderView } from "../state/types";

type Kind = "openai" | "anthropic";

const KIND_LABEL: Record<Kind, string> = {
  openai: "OpenAI-compatible",
  anthropic: "Anthropic",
};

// Official default endpoints — shown as placeholder when Base URL is blank.
// Must mirror DEFAULT_OPENAI_BASE / DEFAULT_ANTHROPIC_BASE on the backend.
const DEFAULT_BASE: Record<Kind, string> = {
  openai: "https://api.openai.com",
  anthropic: "https://api.anthropic.com",
};

interface AddFormState {
  label: string;
  kind: Kind;
  baseUrl: string;
  apiKey: string;
  model: string;
}

const EMPTY_ADD: AddFormState = {
  label: "",
  kind: "openai",
  baseUrl: "",
  apiKey: "",
  model: "",
};

export function ProvidersPanel() {
  const pushToast = useStore((s) => s.pushToast);

  const [providers, setProviders] = useState<ProviderView[]>([]);
  const [loading, setLoading] = useState(true);

  // Add-form state.
  const [form, setForm] = useState<AddFormState>(EMPTY_ADD);
  const [probeModels, setProbeModels] = useState<ModelEntry[]>([]);
  const [probing, setProbing] = useState(false);
  const [probeError, setProbeError] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{
    ok: boolean;
    note: string | null;
  } | null>(null);
  const [saving, setSaving] = useState(false);
  // Manual-mode toggle: when the user wants to type a custom model name
  // without going through /v1/models. The backend has always supported
  // arbitrary model strings — the UI just needed to expose the field.
  const [modelManual, setModelManual] = useState(false);

  const refresh = useCallback(async () => {
    const list = await ipc.providerList();
    setProviders(list);
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const list = await ipc.providerList();
        if (!cancelled) setProviders(list);
      } catch (err) {
        if (!cancelled) pushToast({ text: `provider_list: ${err}`, kind: "error" });
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [pushToast]);

  const canProbe = form.kind === "openai" || form.apiKey.trim().length > 0;

  const invalidateProbe = () => {
    if (probeModels.length > 0 || probeError) {
      setProbeModels([]);
      setProbeError(null);
    }
    if (testResult) setTestResult(null);
  };

  const onProbe = async () => {
    setProbing(true);
    setProbeError(null);
    setProbeModels([]);
    setTestResult(null);
    try {
      const models = await ipc.providerProbeModels({
        kind: form.kind,
        base_url: form.baseUrl.trim(),
        api_key: form.apiKey.trim() || undefined,
      });
      setProbeModels(models);
      if (models.length > 0 && !form.model) {
        setForm((f) => ({ ...f, model: models[0].id }));
        setModelManual(false);
      }
    } catch (err) {
      setProbeError(String(err));
    } finally {
      setProbing(false);
    }
  };

  const onTest = async () => {
    if (!form.model.trim()) {
      setTestResult({ ok: false, note: "Pick or type a model name first" });
      return;
    }
    setTesting(true);
    setTestResult(null);
    try {
      const info = await ipc.providerTestConnection();
      setTestResult({ ok: info.ok, note: info.note });
    } catch (err) {
      setTestResult({ ok: false, note: String(err) });
    } finally {
      setTesting(false);
    }
  };

  const resetForm = () => {
    setForm(EMPTY_ADD);
    setProbeModels([]);
    setProbeError(null);
    setTestResult(null);
    setModelManual(false);
  };

  const onAdd = async () => {
    if (!form.label.trim()) {
      pushToast({ text: "Label is required", kind: "error" });
      return;
    }
    if (!form.model.trim()) {
      pushToast({ text: "Model is required (fetch the list or type a name)", kind: "error" });
      return;
    }
    setSaving(true);
    try {
      const created = await ipc.providerCreate({
        label: form.label.trim(),
        kind: form.kind,
        base_url: form.baseUrl.trim(),
        api_key: form.apiKey.trim() || undefined,
      });
      await ipc.providerSetModel(created.id, form.model.trim());
      pushToast({ text: `Provider "${created.label}" added`, kind: "info" });
      resetForm();
      await refresh();
    } catch (err) {
      pushToast({ text: `provider_create: ${err}`, kind: "error" });
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="providers-panel">
      <section className="providers-add">
        <h3 className="providers-h">Add provider</h3>
        <p className="providers-help">
          Route the orchestrator through any OpenAI-compatible or Anthropic
          endpoint. Pick a model from the list, or type any custom name — the
          gateway sends the string verbatim.
        </p>
        <form
          className="providers-form"
          onSubmit={(e) => {
            e.preventDefault();
            void onAdd();
          }}
        >
          <div className="providers-field">
            <label htmlFor="prov-label">Label</label>
            <input
              id="prov-label"
              type="text"
              value={form.label}
              placeholder="My OpenRouter"
              onChange={(e) => setForm((f) => ({ ...f, label: e.target.value }))}
            />
          </div>
          <div className="providers-field">
            <label htmlFor="prov-kind">Format</label>
            <select
              id="prov-kind"
              value={form.kind}
              onChange={(e) => {
                const k = e.target.value as Kind;
                setForm((f) => ({ ...f, kind: k }));
                invalidateProbe();
              }}
            >
              <option value="openai">{KIND_LABEL.openai}</option>
              <option value="anthropic">{KIND_LABEL.anthropic}</option>
            </select>
          </div>
          <div className="providers-field">
            <label htmlFor="prov-base">
              Base URL <span className="providers-optional">(optional)</span>
            </label>
            <input
              id="prov-base"
              type="url"
              value={form.baseUrl}
              placeholder={DEFAULT_BASE[form.kind]}
              onChange={(e) => {
                setForm((f) => ({ ...f, baseUrl: e.target.value }));
                invalidateProbe();
              }}
            />
            <span className="providers-hint">
              Leave blank for the official {KIND_LABEL[form.kind]} endpoint.
              Set a custom URL for self-hosted or third-party gateways.
            </span>
          </div>
          <div className="providers-field">
            <label htmlFor="prov-key">API key</label>
            <input
              id="prov-key"
              type="password"
              autoComplete="off"
              value={form.apiKey}
              placeholder={form.kind === "anthropic" ? "required" : "optional"}
              onChange={(e) => {
                setForm((f) => ({ ...f, apiKey: e.target.value }));
                invalidateProbe();
              }}
            />
            <span className="providers-hint">
              Encrypted on disk with AES-256-GCM. Never logged, never sent to
              the webview after the initial save.
            </span>
          </div>

          <div className="providers-field">
            <div className="providers-field-row">
              <label htmlFor="prov-model">Model</label>
              <button
                type="button"
                className="providers-link"
                onClick={() => setModelManual((m) => !m)}
              >
                {modelManual ? "Pick from list" : "Type custom name"}
              </button>
            </div>
            {modelManual ? (
              <input
                id="prov-model"
                type="text"
                value={form.model}
                placeholder="e.g. gpt-4o, claude-opus-4-5, my-custom-model"
                onChange={(e) => setForm((f) => ({ ...f, model: e.target.value }))}
              />
            ) : probeModels.length === 0 ? (
              <div className="providers-hint">
                Click <strong>Fetch models</strong> to query {form.kind === "openai" ? "/v1/models" : "the model list"}, or
                click <em>Type custom name</em> to enter any model string
                manually.
              </div>
            ) : (
              <select
                id="prov-model"
                value={form.model}
                onChange={(e) => setForm((f) => ({ ...f, model: e.target.value }))}
              >
                {probeModels.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.id}
                  </option>
                ))}
              </select>
            )}
          </div>

          <div className="providers-actions">
            <button
              type="button"
              className="btn"
              disabled={!canProbe || probing}
              onClick={onProbe}
            >
              {probing ? "Fetching…" : "Fetch models"}
            </button>
            <button
              type="button"
              className="btn"
              disabled={testing || !form.model.trim()}
              onClick={onTest}
              title="Send a tiny request to verify the model + key"
            >
              {testing ? "Testing…" : "Test connection"}
            </button>
          </div>

          {probeError && (
            <div className="providers-error" role="alert">
              {probeError}
            </div>
          )}

          {testResult && (
            <div
              className={testResult.ok ? "providers-success" : "providers-error"}
              role="status"
            >
              {testResult.ok
                ? `Connection OK — ${form.model} responded.`
                : `Connection failed: ${testResult.note ?? "unknown error"}`}
            </div>
          )}

          <div className="providers-actions">
            <button
              type="submit"
              className="btn btn--primary"
              disabled={saving || !form.label.trim() || !form.model.trim()}
            >
              {saving ? "Saving…" : "Add provider"}
            </button>
            <button type="button" className="btn" onClick={resetForm}>
              Reset
            </button>
          </div>
        </form>
      </section>

      <section className="providers-list">
        <h3 className="providers-h">Registered providers</h3>
        {loading ? (
          <div className="providers-empty">Loading…</div>
        ) : providers.length === 0 ? (
          <div className="providers-empty">
            No custom providers yet. Add one above to route the orchestrator
            through your own endpoint.
          </div>
        ) : (
          providers.map((p) => (
            <ProviderRow key={p.id} provider={p} onChanged={refresh} />
          ))
        )}
      </section>
    </div>
  );
}

function ProviderRow({
  provider,
  onChanged,
}: {
  provider: ProviderView;
  onChanged: () => Promise<void>;
}) {
  const pushToast = useStore((s) => s.pushToast);
  const [models, setModels] = useState<ModelEntry[]>(
    provider.model ? [{ id: provider.model }] : []
  );
  const [model, setModel] = useState(provider.model);
  const [modelManual, setModelManual] = useState(false);
  const [fetching, setFetching] = useState(false);
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const [editBusy, setEditBusy] = useState(false);
  const [editState, setEditState] = useState({
    label: provider.label,
    baseUrl: provider.base_url,
    apiKey: "",
  });
  const [ping, setPing] = useState<{
    ok: boolean;
    note: string | null;
  } | null>(null);
  const [pinging, setPinging] = useState(false);

  // When the user enters manual mode, the dropdown is replaced with a text
  // input pre-filled with the current model name. They can type any string
  // and hit save to persist it.
  const showManualInput =
    modelManual || (models.length === 0 && model.length > 0);

  const onFetch = async () => {
    setFetching(true);
    try {
      const list = await ipc.providerFetchModels(provider.id);
      setModels(list);
      if (list.length > 0 && !list.some((m) => m.id === model)) {
        setModel(list[0].id);
      }
      setModelManual(false);
      pushToast({ text: `Fetched ${list.length} models`, kind: "info" });
    } catch (err) {
      pushToast({ text: String(err), kind: "error" });
    } finally {
      setFetching(false);
    }
  };

  const onModelChange = async (value: string) => {
    const previous = model;
    setModel(value);
    try {
      await ipc.providerSetModel(provider.id, value);
      await onChanged();
    } catch (err) {
      setModel(previous);
      pushToast({ text: `provider_set_model: ${err}`, kind: "error" });
    }
  };

  const onPing = async () => {
    if (!model) {
      pushToast({ text: "Pick a model before pinging", kind: "error" });
      return;
    }
    setPinging(true);
    setPing(null);
    try {
      const info = await ipc.providerTestConnection();
      setPing({ ok: info.ok, note: info.note });
    } catch (err) {
      setPing({ ok: false, note: String(err) });
    } finally {
      setPinging(false);
    }
  };

  const onActivate = async () => {
    if (!model) {
      pushToast({ text: "Pick a model before activating", kind: "error" });
      return;
    }
    if (provider.kind === "anthropic" && !provider.has_api_key) {
      pushToast({
        text: "Add an API key before activating an Anthropic provider",
        kind: "error",
      });
      return;
    }
    setBusy(true);
    try {
      await ipc.providerSetActive(provider.id);
      pushToast({ text: `"${provider.label}" is now active`, kind: "info" });
      await onChanged();
    } catch (err) {
      pushToast({ text: `provider_set_active: ${err}`, kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  const onDeactivate = async () => {
    setBusy(true);
    try {
      await ipc.providerSetActive("");
      await onChanged();
    } catch (err) {
      pushToast({ text: `provider_set_active: ${err}`, kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  const onDelete = async () => {
    if (
      !confirm(
        `Delete provider "${provider.label}"? Its stored API key will be removed. This cannot be undone.`
      )
    ) {
      return;
    }
    setBusy(true);
    try {
      await ipc.providerDelete(provider.id);
      pushToast({ text: `Removed "${provider.label}"`, kind: "info" });
      await onChanged();
    } catch (err) {
      pushToast({ text: `provider_delete: ${err}`, kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  const onStartEdit = () => {
    setEditState({
      label: provider.label,
      baseUrl: provider.base_url,
      apiKey: "",
    });
    setEditing(true);
  };

  const onSaveEdit = async () => {
    if (!editState.label.trim()) {
      pushToast({ text: "Label is required", kind: "error" });
      return;
    }
    setEditBusy(true);
    try {
      await ipc.providerUpdate({
        id: provider.id,
        label: editState.label.trim(),
        base_url: editState.baseUrl.trim(),
        model,
        api_key: editState.apiKey.trim() || undefined,
      });
      setEditing(false);
      pushToast({ text: `Updated "${editState.label.trim()}"`, kind: "info" });
      await onChanged();
    } catch (err) {
      pushToast({ text: `provider_update: ${err}`, kind: "error" });
    } finally {
      setEditBusy(false);
    }
  };

  const kindLabel = KIND_LABEL[provider.kind as Kind] ?? provider.kind;

  return (
    <div className={`provider-row ${provider.is_active ? "provider-row--active" : ""}`}>
      {editing ? (
        <EditForm
          state={editState}
          onChange={setEditState}
          onCancel={() => setEditing(false)}
          onSubmit={onSaveEdit}
          busy={editBusy}
          hasExistingKey={provider.has_api_key}
        />
      ) : (
        <>
          <div className="provider-row-head">
            <span className="provider-label">{provider.label}</span>
            <span className="provider-kind">{kindLabel}</span>
            {provider.is_active && (
              <span className="provider-active-tag">active</span>
            )}
            {!provider.has_api_key && (
              <span className="provider-nokey" title="No API key stored">
                no key
              </span>
            )}
          </div>
          <div className="provider-base">{provider.base_url}</div>
          {ping && (
            <div
              className={ping.ok ? "providers-success" : "providers-error"}
              role="status"
            >
              {ping.ok
                ? `Connection OK — ${model} responded.`
                : `Connection failed: ${ping.note ?? "unknown error"}`}
            </div>
          )}
          <div className="provider-row-controls">
            {showManualInput ? (
              <input
                className="provider-model-input"
                aria-label={`Model for ${provider.label}`}
                value={model}
                placeholder="e.g. gpt-4o, claude-opus-4-5"
                onChange={(e) => setModel(e.target.value)}
                onBlur={() => {
                  if (model && model !== provider.model) {
                    void onModelChange(model);
                  }
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    (e.target as HTMLInputElement).blur();
                  }
                }}
              />
            ) : (
              <select
                className="provider-model-select"
                aria-label={`Model for ${provider.label}`}
                value={model}
                onChange={(e) => onModelChange(e.target.value)}
              >
                {models.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.id}
                  </option>
                ))}
              </select>
            )}
            <button
              type="button"
              className="providers-link"
              onClick={() => setModelManual((m) => !m)}
              title={
                modelManual
                  ? "Switch to list picker"
                  : "Type a custom model name (no /v1/models needed)"
              }
            >
              {modelManual ? "pick" : "type"}
            </button>
            <button
              type="button"
              className="btn"
              disabled={fetching}
              onClick={onFetch}
              title="Re-fetch the model list from the provider"
            >
              {fetching ? "…" : "Fetch"}
            </button>
            <button
              type="button"
              className="btn"
              disabled={pinging || !model}
              onClick={onPing}
              title="Send a tiny request to verify the model + key"
            >
              {pinging ? "…" : "Test"}
            </button>
            {provider.is_active ? (
              <button
                type="button"
                className="btn"
                disabled={busy}
                onClick={onDeactivate}
              >
                Deactivate
              </button>
            ) : (
              <button
                type="button"
                className="btn btn--primary"
                disabled={busy || !model}
                onClick={onActivate}
              >
                Activate
              </button>
            )}
            <button
              type="button"
              className="btn"
              disabled={busy}
              onClick={onStartEdit}
            >
              Edit
            </button>
            <button
              type="button"
              className="btn btn--danger"
              disabled={busy}
              onClick={onDelete}
            >
              Delete
            </button>
          </div>
        </>
      )}
    </div>
  );
}

function EditForm({
  state,
  onChange,
  onCancel,
  onSubmit,
  busy,
  hasExistingKey,
}: {
  state: { label: string; baseUrl: string; apiKey: string };
  onChange: (next: { label: string; baseUrl: string; apiKey: string }) => void;
  onCancel: () => void;
  onSubmit: () => void;
  busy: boolean;
  hasExistingKey: boolean;
}) {
  return (
    <form
      className="providers-form"
      onSubmit={(e) => {
        e.preventDefault();
        void onSubmit();
      }}
    >
      <div className="providers-field">
        <label>Label</label>
        <input
          type="text"
          value={state.label}
          onChange={(e) => onChange({ ...state, label: e.target.value })}
        />
      </div>
      <div className="providers-field">
        <label>Base URL</label>
        <input
          type="url"
          value={state.baseUrl}
          onChange={(e) => onChange({ ...state, baseUrl: e.target.value })}
        />
        <span className="providers-hint">
          Trailing <code>/v1</code>, <code>/v1/models</code>, etc. is stripped
          automatically.
        </span>
      </div>
      <div className="providers-field">
        <label>API key</label>
        <input
          type="password"
          autoComplete="off"
          value={state.apiKey}
          placeholder={hasExistingKey ? "(unchanged — type to rotate)" : "required"}
          onChange={(e) => onChange({ ...state, apiKey: e.target.value })}
        />
        <span className="providers-hint">
          Leave blank to keep the existing key. Non-empty input rotates it.
        </span>
      </div>
      <div className="providers-actions">
        <button type="submit" className="btn btn--primary" disabled={busy}>
          {busy ? "Saving…" : "Save"}
        </button>
        <button type="button" className="btn" onClick={onCancel} disabled={busy}>
          Cancel
        </button>
      </div>
    </form>
  );
}
