const Foo = ({ title }: { title?: string }) => (
  <span
    title={title}
    className={`px-1 py-px rounded text-[9px] font-medium leading-none color`} /* allow-bare-rounded */
  >
    hello
  </span>
);
console.log(Foo);
