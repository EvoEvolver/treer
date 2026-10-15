export function Help({ onClose }: { onClose: () => void }) {
  return (
    <div id="help" role="dialog" aria-label="Keyboard shortcuts" onClick={onClose}>
      <div className="help-card" onClick={(e) => e.stopPropagation()}>
        <h2>Keyboard shortcuts</h2>
        <dl>
          <dt><kbd>J</kbd> <kbd>K</kbd></dt><dd>Next / previous issue</dd>
          <dt><kbd>↵</kbd> or <kbd>O</kbd></dt><dd>Open the focused issue</dd>
          <dt><kbd>X</kbd></dt><dd>Close the focused issue (undo available)</dd>
          <dt><kbd>E</kbd></dt><dd>Edit the open issue</dd>
          <dt><kbd>C</kbd></dt><dd>New issue</dd>
          <dt><kbd>/</kbd></dt><dd>Search</dd>
          <dt><kbd>Esc</kbd></dt><dd>Back / dismiss</dd>
          <dt><kbd>?</kbd></dt><dd>This help</dd>
        </dl>
      </div>
    </div>
  );
}
