import { useState } from "react";

export function TransmissionLog() {
  const [open, setOpen] = useState(false);

  return (
    <>
      <button
        className="transmission-log__tab"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        aria-controls="transmission-log-drawer"
      >
        Transmission log
      </button>
      {open && (
        <div className="transmission-log__drawer" id="transmission-log-drawer" role="region" aria-label="Transmission log">
          <div className="transmission-log__drawer-header">
            <span>Transmission log</span>
            <button
              className="transmission-log__close"
              onClick={() => setOpen(false)}
              aria-label="Close transmission log"
            >
              ✕
            </button>
          </div>
          <div className="transmission-log__drawer-body">
            <span className="transmission-log__empty">
              No transmissions yet.
            </span>
          </div>
        </div>
      )}
    </>
  );
}
