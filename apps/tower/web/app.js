const state = { stream: null, event: null };
const base = new URL("./", window.location.href);

async function api(path) {
  const response = await fetch(new URL(path, base));
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.json();
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function row(primary, meta, onClick, current) {
  const button = el("button");
  button.type = "button";
  button.setAttribute("aria-current", current ? "true" : "false");
  button.append(el("span", "primary", primary), el("span", "meta", meta));
  button.addEventListener("click", () => onClick(button));
  return button;
}

async function loadStreams() {
  const [{ stats }, { streams }, { findings }] = await Promise.all([
    api("v1/stats"), api("v1/streams?limit=100"), api("v1/findings?limit=50")
  ]);
  document.querySelector("#stats").textContent = `${stats.streams} streams  ${stats.events} events  ${stats.blob_bytes} bytes`;
  const list = document.querySelector("#stream-list");
  list.replaceChildren(...streams.map(stream => row(
    stream.agent_id,
    `${stream.last_sequence} events · ${stream.stream_id.slice(0, 16)}`,
    () => selectStream(stream), state.stream?.stream_id === stream.stream_id
  )));
  const findingList = document.querySelector("#finding-list");
  findingList.replaceChildren(...findings.map(finding => {
    const node = el("div", "finding");
    node.append(el("strong", "", `${finding.severity} · ${finding.kind}`), el("span", "meta", finding.summary));
    return node;
  }));
}

async function selectStream(stream) {
  state.stream = stream;
  state.event = null;
  document.querySelector("#detail").textContent = "Select an event.";
  document.querySelector("#empty").hidden = true;
  const { events } = await api(`v1/streams/${encodeURIComponent(stream.stream_id)}/events?limit=500`);
  const list = document.querySelector("#event-list");
  list.replaceChildren(...events.map(event => row(
    `${event.sequence}. ${event.method || event.direction}`,
    `${event.direction} · ${event.occurred_at}`,
    button => selectEvent(event, button), false
  )));
  await loadStreams();
}

function selectEvent(event, selected) {
  state.event = event;
  document.querySelector("#detail").textContent = JSON.stringify(event, null, 2);
  for (const button of document.querySelectorAll("#event-list button")) button.setAttribute("aria-current", "false");
  selected.setAttribute("aria-current", "true");
}

loadStreams().catch(error => {
  document.querySelector("#detail").textContent = `Unable to load TOWER: ${error.message}`;
});
