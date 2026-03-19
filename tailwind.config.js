/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{js,ts,jsx,tsx}"],
  theme: {
    extend: {
      colors: {
        text: "var(--color-text)",
        background: "var(--color-background)",
        "logo-primary": "var(--color-logo-primary)",
        "logo-stroke": "var(--color-logo-stroke)",
        "text-stroke": "var(--color-text-stroke)",
      },
      keyframes: {
        indeterminate: {
          "0%": { transform: "translateX(-100%)" },
          "100%": { transform: "translateX(400%)" },
        },
      },
      animation: {
        indeterminate: "indeterminate 1.4s ease-in-out infinite",
      },
    },
  },
  plugins: [],
};
