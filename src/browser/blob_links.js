// A blob URL needs its source context. Let WebKit follow the actual link into a new
// view, where the UI delegate uses the click's modifiers to keep it behind.
(() => {
    function openBehind(event) {
        const modified = event.type === "click" ? event.metaKey && event.button === 0 : event.button === 1;
        if (!event.isTrusted || event.defaultPrevented || !modified) return;

        const link = event.composedPath().find(node => node instanceof Element && node.matches("a[href], area[href]"));
        if (!link || link.hasAttribute("download")) return;
        if (URL.parse(link.getAttribute("href"), link.baseURI)?.protocol !== "blob:") return;

        const target = link.getAttribute("target");
        if (target?.toLowerCase() === "_blank") return;
        link.setAttribute("target", "_blank");
        // Restore after the click's default action has read the target.
        setTimeout(() => {
            if (link.getAttribute("target") !== "_blank") return;
            if (target === null) link.removeAttribute("target");
            else link.setAttribute("target", target);
        }, 0);
    }

    window.addEventListener("click", openBehind);
    window.addEventListener("auxclick", openBehind);
})();
