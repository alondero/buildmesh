/**
 * Per-surface prefix for the `data-dropdown-for` attribute (issue #1264).
 *
 * `useClickOutside` builds its selector from the value passed to it
 * (`[data-dropdown-for="<value>"]`), and each dropdown container
 * mirrors that same value onto the attribute. When two surfaces share
 * the same numeric id (e.g. `mesh.id === node.id` — both autoincrement
 * from the same SQLite sequence), an outside click intended for one
 * menu can satisfy the other's "inside" check, silently closing the
 * wrong dropdown.
 *
 * The fix is to prefix every dropdown id with a surface tag so the
 * namespaces don't collide. Adding the prefix at every render site
 * rather than inside a shared hook keeps the DOM attribute a literal
 * string the reader can grep — and lets the prefix travel with the
 * component (a sidebar menu always emits `mesh-…`, a node context
 * menu always emits `node-…`).
 *
 * Usage:
 * ```ts
 * useClickOutside<string>(open ? dropdownId('mesh', mesh.id) : null, close);
 * ...
 * <div data-dropdown-for={dropdownId('mesh', mesh.id)} />
 * ```
 *
 * The helper is intentionally a tiny pure string operation (no
 * React-specific concerns, no class state) so it's safe to import
 * from any component and to test in isolation.
 */
export function dropdownId(surface: string, id: number | string): string {
  return `${surface}-${id}`;
}