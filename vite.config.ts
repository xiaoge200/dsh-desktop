import { defineConfig } from "vite";
import { resolve } from "path";


const host = process.env.TAURI_DEV_HOST;


export default defineConfig(async () => ({
  
  build: {
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        settings: resolve(__dirname, "settings.html"),
      },
    },
  },

  
  
  
  clearScreen: false,
  
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      
      ignored: [
        "**/src-tauri/**",
        
        
        "**/.*.tmpdir",
        "**/.*.tmpdir/**",
      ],
    },
  },
}));
