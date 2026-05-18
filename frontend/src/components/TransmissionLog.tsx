import { useState } from "react";

export function TransmissionLog() {
  const [open, setOpen] = useState(false);

  return (
    <>
      <button
        className="transmission-log__tab"
        onClick={() => setOpen(!open)}
      >
        Transmission log
      </button>
      {open && (
        <div className="transmission-log__drawer">
          <div className="transmission-log__drawer-header">
            <span>Transmission log</span>
            <button
              className="transmission-log__close"
              onClick={() => setOpen(false)}
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
