// Observe the scroll box and its content so the fade follows pagination and streaming.
export function bindScrollEdges(scroller) {
  const edge = scroller.parentElement;
  let frame;
  function update() {
    if (frame) return;
    frame = requestAnimationFrame(() => {
      frame = undefined;
      const bottom = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
      edge.toggleAttribute('data-more-below', scroller.clientHeight > 0 && bottom > 1);
    });
  }
  const observer = new ResizeObserver(update);
  observer.observe(scroller);
  for (const content of scroller.children) observer.observe(content);
  scroller.addEventListener('scroll', update, { passive: true });
  update();
}
