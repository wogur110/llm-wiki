/** @type {import('next').NextConfig} */
const nextConfig = {
  // Tauri expects a fully static export — no Node.js server at runtime
  output: "export",

  // Disable Next.js built-in image optimisation (incompatible with static export)
  images: {
    unoptimized: true,
  },
};

export default nextConfig;
