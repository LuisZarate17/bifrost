// The shared renderer, used by Heimdall's popup and (from G1) by Mímir.
//
// Extracted at Before-G0 step 1e, deliberately while Heimdall was still its only
// consumer: a move you can verify against one app is far safer than the same
// move with two.
//
// The stylesheet is NOT imported here. Consumers do `import "@odin/ui/answer.css"`
// themselves, because `npm run check:inert` builds Answer for SSR and runs the
// output under plain node, where a CSS import would fail outright.

export { Answer } from "./Answer";
export { CodeBlock } from "./CodeBlock";
