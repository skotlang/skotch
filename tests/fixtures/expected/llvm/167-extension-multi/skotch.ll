; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.true = private unnamed_addr constant [5 x i8] c"true\00", align 1
@.str.false = private unnamed_addr constant [6 x i8] c"false\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)
declare i32 @printf(ptr, ...)

define i32 @InputKt_isPositive(i32 %arg0) {
entry:
  %t0 = add i32 0, 0
  %t1 = icmp sgt i32 %arg0, %t0
  %t2 = zext i1 %t1 to i32
  ret i32 %t2
}

define i32 @InputKt_negate(i32 %arg0) {
entry:
  %t0 = add i32 0, 0
  %t1 = sub i32 %t0, %arg0
  ret i32 %t1
}

define i32 @InputKt_doubled(i32 %arg0) {
entry:
  %t0 = add i32 0, 2
  %t1 = mul i32 %arg0, %t0
  ret i32 %t1
}

define i32 @main() {
entry:
  %t0 = add i32 0, 5
  %t1 = call i32 @InputKt_isPositive(i32 %t0)
  %t3 = trunc i32 %t1 to i1
  %t4 = select i1 %t3, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t4)
  %t6 = add i32 0, -3
  %t7 = call i32 @InputKt_isPositive(i32 %t6)
  %t9 = trunc i32 %t7 to i1
  %t10 = select i1 %t9, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t10)
  %t12 = add i32 0, 5
  %t13 = call i32 @InputKt_negate(i32 %t12)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t13)
  %t15 = add i32 0, 7
  %t16 = call i32 @InputKt_doubled(i32 %t15)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t16)
  ret i32 0
}

