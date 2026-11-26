'use strict';

function diagramTheme({ dark, palette }) {
  const { paper, ink, muted, active, raised } = palette || {};
  if (![paper, ink, muted, active, raised].every((color) => /^#[0-9a-f]{6}$/i.test(color))) {
    return { theme: dark ? 'dark' : 'neutral' };
  }
  return {
    theme: 'base',
    themeVariables: {
      darkMode: Boolean(dark), background: paper, textColor: ink, titleColor: ink, fontSize: '14px',
      primaryColor: active, primaryTextColor: ink, primaryBorderColor: raised,
      tertiaryColor: paper, tertiaryTextColor: ink, lineColor: muted,
      clusterBkg: paper, clusterBorder: raised, edgeLabelBackground: paper,
    },
  };
}

window.addEventListener('message', async (event) => {
  if (window.parent === window || event.source !== window.parent || event.origin !== location.origin) return;
  const reply = event.ports[0];
  const text = event.data?.text;
  if (!reply || typeof text !== 'string' || text.length > 24_000) return;
  try {
    mermaid.initialize({
      startOnLoad: false, securityLevel: 'strict', suppressErrorRendering: true,
      look: 'classic', fontFamily: 'system-ui, sans-serif',
      themeCSS: '.node rect.basic.label-container { rx: 6px; ry: 6px; } .nodeLabel p { line-height: 1.3; }',
      flowchart: { diagramPadding: 10, nodeSpacing: 22, rankSpacing: 32, padding: 10, wrappingWidth: 205, curve: 'rounded' },
      maxTextSize: 24_000, maxEdges: 500, ...diagramTheme(event.data),
    });
    const { svg } = await mermaid.render('diagram', text);
    const template = document.createElement('template');
    template.innerHTML = svg;
    const image = template.content.firstElementChild;
    if (image?.localName !== 'svg') throw new Error('Missing diagram image');
    // Mermaid can emit HTML labels with void elements. Standalone SVG images require XML.
    const { width, height } = image.viewBox.baseVal;
    reply.postMessage({ svg: new XMLSerializer().serializeToString(image), width, height });
  } catch {
    reply.postMessage(null);
  } finally {
    reply.close();
  }
});
