'use strict';

window.addEventListener('message', async (event) => {
  if (window.parent === window || event.source !== window.parent || event.origin !== location.origin) return;
  const reply = event.ports[0];
  const text = event.data?.text;
  if (!reply || typeof text !== 'string' || text.length > 24_000) return;
  try {
    mermaid.initialize({
      startOnLoad: false, securityLevel: 'strict', suppressErrorRendering: true,
      theme: event.data.theme === 'dark' ? 'dark' : 'default', maxTextSize: 24_000, maxEdges: 500,
    });
    const { svg } = await mermaid.render('diagram', text);
    const template = document.createElement('template');
    template.innerHTML = svg;
    const image = template.content.firstElementChild;
    if (image?.localName !== 'svg') throw new Error('Missing diagram image');
    // Mermaid can emit HTML labels with void elements. Standalone SVG images require XML.
    reply.postMessage({ svg: new XMLSerializer().serializeToString(image), width: image.viewBox.baseVal.width });
  } catch {
    reply.postMessage(null);
  } finally {
    reply.close();
  }
});
