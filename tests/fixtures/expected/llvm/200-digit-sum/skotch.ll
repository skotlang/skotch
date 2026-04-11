; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @InputKt_digitSum(i32 %arg0) {
entry:
  %merge_4 = alloca i32
  %merge_9 = alloca i32
  %merge_11 = alloca i32
  %t0 = add i32 0, 0
  %t1 = icmp slt i32 %arg0, %t0
  br i1 %t1, label %bb1, label %bb2
bb1:
  %t2 = add i32 0, 0
  %t3 = sub i32 %t2, %arg0
  store i32 %t3, ptr %merge_4
  br label %bb3
bb2:
  store i32 %arg0, ptr %merge_4
  br label %bb3
bb3:
  %t4 = load i32, ptr %merge_4
  store i32 %t4, ptr %merge_9
  %t5 = add i32 0, 0
  store i32 %t5, ptr %merge_11
  br label %bb4
bb4:
  %t6 = load i32, ptr %merge_9
  %t7 = add i32 0, 0
  %t8 = icmp sgt i32 %t6, %t7
  br i1 %t8, label %bb5, label %bb6
bb5:
  %t9 = load i32, ptr %merge_11
  %t10 = load i32, ptr %merge_9
  %t11 = add i32 0, 10
  %t12 = srem i32 %t10, %t11
  %t13 = add i32 %t9, %t12
  store i32 %t13, ptr %merge_11
  %t14 = load i32, ptr %merge_9
  %t15 = add i32 0, 10
  %t16 = sdiv i32 %t14, %t15
  store i32 %t16, ptr %merge_9
  br label %bb4
bb6:
  %t17 = load i32, ptr %merge_11
  ret i32 %t17
}

define i32 @main() {
entry:
  %t0 = add i32 0, 123
  %t1 = call i32 @InputKt_digitSum(i32 %t0)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t1)
  %t3 = add i32 0, 9999
  %t4 = call i32 @InputKt_digitSum(i32 %t3)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t4)
  %t6 = add i32 0, 0
  %t7 = call i32 @InputKt_digitSum(i32 %t6)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t7)
  ret i32 0
}

