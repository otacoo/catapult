import { useEffect, useRef } from "react";
import { useLocation } from "react-router-dom";
import { useChatUrl } from "../chatState";

export default function ChatIframeHost() {
  const location = useLocation();
  const url = useChatUrl();
  const isChat = location.pathname === "/chat";
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!iframeRef.current) {
      const iframe = document.createElement("iframe");
      iframe.className = "border-0 w-full h-full";
      iframe.allow = "clipboard-write";
      iframe.title = "llama.cpp Chat";
      iframeRef.current = iframe;
    }
    if (!containerRef.current) {
      const container = document.createElement("div");
      container.id = "chat-iframe-container";
      container.style.position = "fixed";
      container.style.top = "2.75rem";
      container.style.left = "0";
      container.style.right = "0";
      container.style.bottom = "0";
      container.style.display = "none";
      container.style.flexDirection = "column";
      container.style.background = "#0f0f0f";
      container.style.zIndex = "10";
      // Header for URL
      const header = document.createElement("div");
      header.id = "chat-iframe-header";
      header.style.display = "flex";
      header.style.alignItems = "center";
      header.style.justifyContent = "space-between";
      header.style.padding = "0.5rem 1rem";
      header.style.borderBottom = "1px solid #2a2a2a";
      header.style.background = "#0f0f0f";
      header.style.fontSize = "0.75rem";
      header.style.color = "#6b7280";
      header.style.fontFamily = "monospace";
      container.appendChild(header);
      container.appendChild(iframeRef.current!);
      document.body.appendChild(container);
      containerRef.current = container;
    }
  }, []);

  useEffect(() => {
    const iframe = iframeRef.current;
    const container = containerRef.current;
    if (!iframe || !container) return;
    const header = document.getElementById("chat-iframe-header");
    if (header) header.textContent = url || "";
    if (isChat && url) {
      if (iframe.src !== url) {
        iframe.src = url;
      }
      container.style.display = "flex";
    } else {
      container.style.display = "none";
    }
  }, [isChat, url]);

  return null;
}
