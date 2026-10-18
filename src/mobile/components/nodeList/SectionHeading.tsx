

export function SectionHeading({
  children,
  color,
}: {
  children: React.ReactNode;
  color: string;
}) {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        padding: "10px 12px 6px",
      }}
    >
      <span
        style={{
          fontSize: 10,
          fontWeight: 600,
          color,
          textTransform: "uppercase",
          letterSpacing: "0.05em",
        }}
      >
        {children}
      </span>
    </div>
  );
}
