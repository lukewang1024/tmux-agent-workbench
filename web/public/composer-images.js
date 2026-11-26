import { button, glyph, make } from './dom.js?v=WORKBENCH_CACHE_VERSION';

const maximum = 3;
const maxBytes = 256 * 1024;
const originalTypes = new Set(['image/png', 'image/jpeg', 'image/webp']);

function dataURL(blob) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.addEventListener('load', () => resolve(reader.result), { once: true });
    reader.addEventListener('error', () => reject(reader.error), { once: true });
    reader.readAsDataURL(blob);
  });
}

async function prepareImage(file) {
  if (!file.type.startsWith('image/') || file.type === 'image/svg+xml' || file.size > 30 * 1024 * 1024) throw new Error();
  const bitmap = await createImageBitmap(file);
  try {
    let encoded = file;
    if (!originalTypes.has(file.type) || file.size > maxBytes || Math.max(bitmap.width, bitmap.height) > 1600) {
      const canvas = make('canvas');
      const context = canvas.getContext('2d', { alpha: false });
      for (const edge of [1600, 1200, 880, 600]) {
        const scale = Math.min(1, edge / Math.max(bitmap.width, bitmap.height));
        canvas.width = Math.max(1, Math.round(bitmap.width * scale));
        canvas.height = Math.max(1, Math.round(bitmap.height * scale));
        context.fillStyle = '#fff';
        context.fillRect(0, 0, canvas.width, canvas.height);
        context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
        encoded = await new Promise((resolve) => canvas.toBlob(resolve, 'image/jpeg', .88));
        if (encoded?.size <= maxBytes) break;
      }
    }
    if (!encoded || encoded.size > maxBytes) throw new Error();
    return { mime: encoded.type, name: file.name, content: (await dataURL(encoded)).split(',', 2)[1] };
  } finally { bitmap.close(); }
}

export function imagePreview(image, fallback) {
  const element = make('img');
  element.src = image.src || 'data:' + image.mime + ';base64,' + image.content;
  element.alt = image.name || fallback;
  return element;
}

export function mountComposerImages(attach, picker, input, strip, { t, context, getImages, setImages, canAttach, onUpdate, onError }) {
  const pending = new Set();
  let rendered;
  async function add(files) {
    const key = context();
    if (!key || !files.length || pending.has(key) || !canAttach()) return;
    const available = maximum - getImages(key).length;
    if (files.length > available) onError(t('imageAttachmentLimit'));
    if (!available) return;
    pending.add(key);
    onUpdate();
    try {
      const added = await Promise.allSettled(files.slice(0, available).map(prepareImage));
      const images = added.flatMap((result) => result.status === 'fulfilled' ? [result.value] : []);
      if (images.length) setImages(key, [...getImages(key), ...images].slice(0, maximum));
      if (images.length < added.length) onError(t('imageAttachmentUnreadable'));
    } finally {
      pending.delete(key);
      onUpdate();
    }
  }

  function paint(key) {
    const images = key ? getImages(key) : [];
    attach.hidden = !key;
    attach.disabled = !key || !canAttach() || pending.has(key) || images.length >= maximum;
    if (rendered === images) return;
    rendered = images;
    strip.hidden = !images.length;
    strip.replaceChildren(...images.map((image, index) => {
      const card = make('div', 'draft-image');
      const remove = button('', 'remove-image', () => {
        setImages(key, getImages(key).filter((_, position) => position !== index));
        input.focus({ preventScroll: true });
      });
      remove.setAttribute('aria-label', t('removeImage'));
      remove.title = t('removeImage');
      remove.append(glyph('close'));
      card.append(imagePreview(image, t('attachedImage')), remove);
      return card;
    }));
  }

  attach.addEventListener('click', () => picker.click());
  picker.addEventListener('change', () => {
    const files = [...picker.files];
    picker.value = '';
    add(files);
  });
  input.addEventListener('paste', (event) => {
    const files = [...event.clipboardData?.files || []].filter((file) => file.type.startsWith('image/'));
    if (!files.length || !context() || !canAttach()) return;
    event.preventDefault();
    add(files);
  });
  return { paint, pending: (key) => pending.has(key) };
}
