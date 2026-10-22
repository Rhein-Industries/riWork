// Runs only inside the offline, disposable WKWebView worker.
async function riworkRender(source, dark) {
  const reply = value => window.webkit.messageHandlers.result.postMessage(value);
  try {
    mermaid.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      suppressErrorRendering: true,
      maxTextSize: 32768,
      maxEdges: 500,
      htmlLabels: false,
      flowchart: { htmlLabels: false },
      theme: dark ? "dark" : "default",
      fontFamily: "Arial, sans-serif",
      // Diagram directives cannot weaken isolation or inject arbitrary theme CSS.
      secure: ["secure", "securityLevel", "startOnLoad", "maxTextSize", "maxEdges",
        "suppressErrorRendering", "htmlLabels", "themeCSS", "fontFamily"],
    });
    const result = await mermaid.render("riworkDiagram", source);
    const doc = new DOMParser().parseFromString(result.svg, "image/svg+xml");
    const svg = doc.documentElement;
    // Defense in depth: the output is a noninteractive PNG and may never fetch.
    for (const node of svg.querySelectorAll("script,image,img,iframe,object,embed")) node.remove();
    for (const node of svg.querySelectorAll("a")) node.replaceWith(...node.childNodes);
    for (const node of [svg, ...svg.querySelectorAll("*")]) {
      for (const attr of [...node.attributes]) {
        if (/^on/i.test(attr.name) || /^(href|xlink:href|src|srcset)$/i.test(attr.name)) node.removeAttribute(attr.name);
      }
      if (node.tagName.toLowerCase() === "style") node.textContent = node.textContent.replace(/url\(\s*(?!['"]?#)[^)]*\)/gi, "none").replace(/@import[^;]*;/gi, "");
      if (node.hasAttribute("style")) node.setAttribute("style", node.getAttribute("style").replace(/url\(\s*(?!['"]?#)[^)]*\)/gi, "none"));
    }
    const box = svg.getAttribute("viewBox")?.trim().split(/[\s,]+/).map(Number);
    const width = box?.length === 4 ? box[2] : parseFloat(svg.getAttribute("width"));
    const height = box?.length === 4 ? box[3] : parseFloat(svg.getAttribute("height"));
    if (!(width > 0 && height > 0 && Number.isFinite(width) && Number.isFinite(height))) throw Error("The diagram has no usable dimensions.");
    // At most 2048 per axis and 4 Mi pixels. Preserve aspect ratio; viewer can zoom.
    const scale = Math.min(2, 2048 / width, 2048 / height);
    const w = Math.max(1, Math.ceil(width * scale)), h = Math.max(1, Math.ceil(height * scale));
    svg.setAttribute("width", String(w));
    svg.setAttribute("height", String(h));
    svg.setAttribute("xmlns", "http://www.w3.org/2000/svg");
    svg.style.maxWidth = "none";
    const image = new Image();
    image.onload = () => {
      try {
        const canvas = document.createElement("canvas"); canvas.width = w; canvas.height = h;
        const ctx = canvas.getContext("2d");
        ctx.fillStyle = dark ? "#17191e" : "#ffffff"; ctx.fillRect(0, 0, w, h);
        ctx.drawImage(image, 0, 0, w, h);
        const png = canvas.toDataURL("image/png").split(",")[1];
        if (png.length > 6 * 1024 * 1024) throw Error("The rendered diagram is too large.");
        reply({ png, width: w, height: h });
      } catch (error) { reply({ error: String(error.message || error).slice(0, 1000) }); }
    };
    image.onerror = () => reply({ error: "The diagram could not be converted to an image." });
    image.src = "data:image/svg+xml;charset=utf-8," + encodeURIComponent(new XMLSerializer().serializeToString(svg));
  } catch (error) {
    reply({ error: String(error.message || error).slice(0, 1000) });
  }
}
