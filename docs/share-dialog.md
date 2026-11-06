# Share dialog component

`crates/soil-search-host/web/soil-share-dialog.js` is a presentation-only custom
element for sharing any resource.

```text
resource    record | file | document | sheet | slide | content
target      team | folder | principal | link
role        viewer | commenter | editor
projection  default | full | contact-card | ...
```

The component does not grant, revoke or store ACL state. It emits intents; the
host owns Mycelium/Unibus grants, folder policy, projections and audit.

## Usage

```html
<script type="module" src="/web/soil-share-dialog.js"></script>

<soil-share-dialog
  targets-endpoint="/api/share-targets"
  roles="viewer,commenter,editor"
  projections="default,contact-card">
</soil-share-dialog>
```

```js
const dialog = document.querySelector("soil-share-dialog");
dialog.shares = await fetch(`/api/shares?resource=${encodeURIComponent(token)}`).then(r => r.json());
dialog.show({
  kind: "contact",
  token: "record:personal/avery/contacts#c-ada",
  label: "Ada Lovelace",
});
```

## Events

```text
soil-share-request
  detail { resource, targets[], role, projection }

soil-share-revoke
  detail { resource, share }
```

Both are cancelable. Cancel to handle the operation yourself.

## Target contract

Targets use the same candidate shape as `soil-mention-input`:

```text
kind
token
label
subtitle?
visibility?
```

Examples:

```text
team:fpl
folder:team/fpl/crm/accounts/a-acme
principal:oidc:https://issuer#subject
```

## Composition

```js
dialog.renderTarget = (target, element, dialog) => {
  element.textContent = target.label;
};

dialog.transformTargetRequest = ({ url, query }) => ({
  url: url + "&scope=shareable",
  query,
});
```

## Theming

CSS custom properties:

```css
soil-share-dialog {
  --soil-share-bg: #12171c;
  --soil-share-fg: #dce5ea;
  --soil-share-muted: #8395a2;
  --soil-share-border: #26313b;
  --soil-share-accent: #7dd3a8;
  --soil-share-radius: 8px;
  --soil-share-shadow: 0 12px 32px rgba(0,0,0,.35);
  --soil-share-font: ui-sans-serif, system-ui, sans-serif;
}
```

Shadow parts:

```text
title resource input targets target target-kind chip
```

## Consistency with mentions

Mention/link picking and share targets both use:

```text
kind
token
label
subtitle?
```

The token is identity. The label is display. A host can use the same
`/api/suggest` backend for contacts, documents, teams, folders and principals,
with different `kinds` filters.
