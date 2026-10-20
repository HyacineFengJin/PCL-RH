// Deterministic hook/effect scheduler for the actual compiled components. This
// is not a second implementation of their business logic or a DOM/browser test.
let active;
export const Fragment = Symbol('fragment');
export const jsx = (type, props, key) => ({ type, props: props ?? {}, key });
export const jsxs = jsx;
function hook() {
  if (!active) throw new Error('Hook used outside the component event harness.');
  return [active, active.cursor++];
}
export function useRef(initial) {
  const [fiber, index] = hook();
  return fiber.hooks[index] ??= { current: initial };
}
export function useState(initial) {
  const [fiber, index] = hook();
  const state = fiber.hooks[index] ??= { value: typeof initial === 'function' ? initial() : initial };
  return [state.value, next => {
    if (!fiber.mounted) { fiber.retiredWrites++; return; }
    const value = typeof next === 'function' ? next(state.value) : next;
    if (!Object.is(value, state.value)) { state.value = value; fiber.dirty = true; }
  }];
}
export function useEffect(setup, dependencies) {
  const [fiber, index] = hook();
  const previous = fiber.hooks[index];
  if (previous && dependencies && dependencies.length === previous.dependencies?.length &&
      dependencies.every((value, i) => Object.is(value, previous.dependencies[i]))) return;
  const effect = { setup, dependencies, cleanup: undefined };
  fiber.hooks[index] = effect;
  fiber.effects.push({ previous, effect });
}
export function mount(Component, initialProps) {
  const fiber = { hooks: [], effects: [], mounted: true, cursor: 0, dirty: false, retiredWrites: 0 };
  let props = initialProps, tree;
  function render() {
    if (!fiber.mounted) throw new Error('Cannot render an unmounted event harness.');
    fiber.cursor = 0; fiber.dirty = false; active = fiber;
    try { tree = Component(props); } finally { active = undefined; }
    const effects = fiber.effects.splice(0);
    for (const { previous } of effects) previous?.cleanup?.();
    for (const { effect } of effects) effect.cleanup = effect.setup();
    return tree;
  }
  render();
  return {
    get tree() { return tree; },
    get retiredWrites() { return fiber.retiredWrites; },
    setProps(next) { props = next; render(); },
    async flush() {
      for (let i = 0; i < 16; i++) {
        await Promise.resolve();
        if (fiber.dirty && fiber.mounted) render();
      }
    },
    restartEffects() {
      const effects = fiber.hooks.filter(value => value?.setup);
      for (const effect of effects) effect.cleanup?.();
      for (const effect of effects) effect.cleanup = effect.setup();
    },
    unmount() {
      if (!fiber.mounted) return;
      fiber.mounted = false;
      for (const effect of fiber.hooks) effect?.cleanup?.();
    },
  };
}
export function nodes(tree) {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree || typeof tree !== 'object' || !tree.props) return [];
  return [tree, ...nodes(tree.props.children)];
}
export function text(tree) {
  if (Array.isArray(tree)) return tree.map(text).join('');
  if (tree == null || typeof tree === 'boolean') return '';
  return typeof tree === 'object' ? text(tree.props?.children) : String(tree);
}
// Trusted CE child controls are inspected at their public props. These helpers
// let actual card modules bundle without replacing their field event handlers.
export const Children = { toArray: value => value == null ? [] : Array.isArray(value) ? value.flat(Infinity) : [value] };
export const isValidElement = value => !!value && typeof value === 'object' && 'props' in value;
export const useId = () => useRef('extension-test-control').current;
export const useLayoutEffect = useEffect;
export const createContext = value => ({ value });
export const useContext = context => context.value;
export const createPortal = children => children;
export const forwardRef = render => props => render(props, null);
