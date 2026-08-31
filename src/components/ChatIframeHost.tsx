import { useEffect, useRef } from "react";
import { useLocation } from "react-router-dom";
import { useChatUrl } from "../chatState";

export default function ChatIframeHost() {
  const location = useLocation();
  const url = useChatUrl();
  const isChat = location.pathname === "/chat";
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const hiddenHostRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!iframeRef.current) {
      const iframe = document.createElement("iframe");
      iframe.className = "border-0 w-full h-full";
      iframe.allow = "clipboard-write";
      iframe.title = "llama.cpp Chat";
      iframeRef.current = iframe;
    }
    if (!hiddenHostRef.current) {
      const host = document.createElement("div");
      host.style.display = "none";
      document.body.appendChild(host);
      hiddenHostRef.current = host;
      // Keep iframe in hidden host initially
      if (iframeRef.current && !iframeRef.current.parentNode) {
        hiddenHostRef.current.appendChild(iframeRef.current);
      }
    }
  }, []);

  useEffect(() => {
    const iframe = iframeRef.current;
    if (!iframe) return;
    if (isChat && url) {
      if (iframe.src !== url) {
        iframe.src = url;
      }
      const moveToPlaceholder = () => {
        const placeholder = document.getElementById("chat-iframe-placeholder");
        if (placeholder) {
          if (iframe.parentNode !== placeholder) {
            placeholder.appendChild(iframe);
          }
          iframe.style.display = "block";
        } else {
          // Placeholder not yet mounted, retry next frame
          requestAnimationFrame(moveToPlaceholder);
        }
      };
      moveToPlaceholder();
    } else {
      // Move iframe to hidden host to keep it alive
      if (hiddenHostRef.current && iframe.parentNode !== hiddenHostRef.current) {
        hiddenHostRef.current.appendChild(iframe);
      }
      iframe.style.display = "block";
      // Hidden host is display:none, so iframe is not visible
    }
  }, [isChat, url]);

  return null;
}
