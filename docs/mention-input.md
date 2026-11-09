# Mention input component

`crates/soil-search-host/web/soil-mention-input.js` is a small custom element over
`/api/suggest`.

It is intentionally presentation-only:

```text
no policy
no direct records/VFS access
no token storage
no hard-coded entity kinds
```

## Usage

```html
<script type="module" src="/web/soil-mention-input.js"></script>

<soil-mention-input
  endpoint="/api/suggest"
  kinds="person,account,project,task,document,sheet,slide,file"
  limit="10"
  placeholder="Type @ to mention">
</soil-mention-input>
```

On selection it replaces the current `@query` with the candidate token and emits:

```text
soil-mention-query   { text, start, end } | null
soil-mention-select  { candidate, token, replace }
```

Cancel `soil-mention-select` to insert the token yourself:

```js
input.addEventListener("soil-mention-select", event => {
  if (event.detail.candidate.kind === "file") {
    event.preventDefault();
    insertMyOwnReference(event.detail.candidate);
  }
});
```

## Theming

CSS custom properties:

```css
soil-mention-input {
  --soil-mention-bg: #12171c;
  --soil-mention-fg: #dce5ea;
  --soil-mention-muted: #8395a2;
  --soil-mention-border: #26313b;
  --soil-mention-accent: #7dd3a8;
  --soil-mention-radius: 8px;
  --soil-mention-shadow: 0 12px 32px rgba(0,0,0,.35);
  --soil-mention-font: ui-sans-serif, system-ui, sans-serif;
}
```

Shadow parts:

```text
input popup item kind label subtitle
```

## Composition

Replace the default item renderer:

```js
const input = document.querySelector("soil-mention-input");
input.renderCandidate = (candidate, item) => {
  item.innerHTML = `<strong>${candidate.label}</strong>`;
};
```

Transform the request before fetch:

```js
input.transformRequest = ({ url, query }) => ({
  url: url + "&scope=team",
  query,
});
```

Slots:

```text
<slot name="prefix"> before the input
```

## Candidate contract

The component only relies on:

```text
kind
token
label
subtitle?
```

That keeps it compatible with records, VFS, Marbles and principals.

## Shared selector contract

The same `{ kind, token, label, subtitle? }` candidate contract is used by the
share target picker (`soil-share-dialog`) and by mention/link insertion. A Spore
should not invent a second entity/token format for sharing.

For contacts, documents, sheets and slides, the inserted token is the stable
locator; labels are display only.
