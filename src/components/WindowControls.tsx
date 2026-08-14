import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, Square, Layers, X } from "lucide-react";

export default function WindowControls() {
  const appWindow = getCurrentWindow();
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    let debounce: ReturnType<typeof setTimeout>;
    const unlisten = appWindow.onResized(() => {
      clearTimeout(debounce);
      debounce = setTimeout(() => {
        appWindow.isMaximized().then(setMaximized);
      }, 100);
    });
    return () => {
      clearTimeout(debounce);
      unlisten.then((f) => f());
    };
  }, []);

  return (
    <div className="flex items-center">
      <button
        onClick={() => appWindow.minimize()}
        className="w-10 h-11 flex items-center justify-center text-gray-400 hover:text-gray-200 hover:bg-white/5 transition-colors"
        title="Minimize"
      >
        <Minus size={14} />
      </button>
      <button
        onClick={() => appWindow.toggleMaximize()}
        className="w-10 h-11 flex items-center justify-center text-gray-400 hover:text-gray-200 hover:bg-white/5 transition-colors"
        title={maximized ? "Restore" : "Maximize"}
      >
        {maximized ? <Layers size={12} /> : <Square size={11} />}
      </button>
      <button
        onClick={() => appWindow.close()}
        className="w-10 h-11 flex items-center justify-center text-gray-400 hover:text-white hover:bg-red-600 transition-colors"
        title="Close"
      >
        <X size={14} />
      </button>
    </div>
  );
}
